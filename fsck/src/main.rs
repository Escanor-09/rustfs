use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use libfs::{
    bitmap::Bitmap,
    dir::DirStore,
    disk::Disk,
    inode::InodeStore,
    layout::{BLOCK_SIZE, Inode, MAGIC, ROOT_INODE, Superblock},
};

fn collect_blocks(
    inode: &Inode,
    inode_num: u64,
    disk: &Arc<Mutex<Disk>>,
    block_owner: &mut HashMap<u64, Vec<u64>>,
    data_start_block: u64,
    total_blocks: u64,
    errors: &mut u32,
) {
    const PTRS_PER_BLOCK: usize = (BLOCK_SIZE as usize) / std::mem::size_of::<u64>();

    let mut record_block = |block: u64| {
        if block == 0 {
            return;
        }

        //Invalid block pointer
        if block < data_start_block || block >= total_blocks {
            println!(
                "INVALID BLOCK POINTER: inode {} points to block {}",
                inode_num, block
            );
            *errors += 1;
            return;
        }

        block_owner.entry(block).or_default().push(inode_num);
    };

    //Direct Blocks
    for &block in &inode.direct {
        record_block(block);
    }

    //Single indirect
    if inode.indirect != 0 {
        record_block(inode.indirect);

        let indirect_block = disk.lock().unwrap().read_block(inode.indirect).unwrap();

        let ptrs = unsafe {
            std::slice::from_raw_parts(indirect_block.as_ptr() as *const u64, PTRS_PER_BLOCK)
        };

        for &block in ptrs {
            record_block(block);
        }
    }

    //Double indrect
    if inode.double_indirect != 0 {
        record_block(inode.double_indirect);

        let outer_block = disk
            .lock()
            .unwrap()
            .read_block(inode.double_indirect)
            .unwrap();

        let outer_ptrs = unsafe {
            std::slice::from_raw_parts(outer_block.as_ptr() as *const u64, PTRS_PER_BLOCK)
        };

        for &inner_block in outer_ptrs {
            if inner_block == 0 {
                continue;
            }

            record_block(inner_block);

            let inner_block_data = disk.lock().unwrap().read_block(inner_block).unwrap();

            let inner_ptrs = unsafe {
                std::slice::from_raw_parts(inner_block_data.as_ptr() as *const u64, PTRS_PER_BLOCK)
            };

            for &data_block in inner_ptrs {
                record_block(data_block);
            }
        }
    }
}

fn walk_directory(
    dir_inode_num: u64,
    parent_inode_num: u64,
    dir_store: &mut DirStore,
    inode_store: &mut InodeStore,
    disk: &Arc<Mutex<Disk>>,
    refrenced_inodes: &mut HashSet<u64>,
    link_counts: &mut HashMap<u64, u32>,
    subdir_counts: &mut HashMap<u64, u32>,
    block_owner: &mut HashMap<u64, Vec<u64>>,
    visited_dirs: &mut HashSet<u64>,
    collected_inodes: &mut HashSet<u64>,
    data_start_block: u64,
    total_blocks: u64,
    errors: &mut u32,
) {
    if visited_dirs.contains(&dir_inode_num) {
        println!(
            "WARNING: directory cycle detected at inode {}",
            dir_inode_num
        );
        return;
    }
    visited_dirs.insert(dir_inode_num);

    let current_inode = inode_store.read_inode(dir_inode_num);
    if collected_inodes.insert(dir_inode_num) {
        collect_blocks(
            &current_inode,
            dir_inode_num,
            disk,
            block_owner,
            data_start_block,
            total_blocks,
            errors,
        );
    }

    let entries = dir_store.list(dir_inode_num);

    for (inode_num, name) in entries {
        let inode_num = inode_num as u64;

        if name == "." {
            if inode_num != dir_inode_num {
                println!(
                    "CORRUPTED DIRECTORY: '.' in inode {} points to inode {}",
                    dir_inode_num, inode_num
                );
                *errors += 1;
            }
            continue;
        }

        if name == ".." {
            if inode_num != parent_inode_num {
                println!(
                    "CORRUPTED DIRECTORY: '..' in inode {} points to inode {} instead of {}",
                    dir_inode_num, inode_num, parent_inode_num
                );
                *errors += 1;
            }
            continue;
        }
        refrenced_inodes.insert(inode_num);
        *link_counts.entry(inode_num).or_insert(0) += 1;

        let inode = inode_store.read_inode(inode_num);

        if collected_inodes.insert(inode_num) {
            collect_blocks(
                &inode,
                inode_num,
                disk,
                block_owner,
                data_start_block,
                total_blocks,
                errors,
            );
        }

        let is_dir = inode.mode & 0o170000 == 0o040000;
        if is_dir {
            *subdir_counts.entry(dir_inode_num).or_insert(0) += 1;
            walk_directory(
                inode_num,
                dir_inode_num,
                dir_store,
                inode_store,
                disk,
                refrenced_inodes,
                link_counts,
                subdir_counts,
                block_owner,
                visited_dirs,
                collected_inodes,
                data_start_block,
                total_blocks,
                errors,
            );
        }
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("Usage: fsck <image>");

    let disk = Disk::open(&path).expect("Failed to open disk image");
    let disk_arc = Arc::new(Mutex::new(disk));

    let sb_block = disk_arc.lock().unwrap().read_block(0).unwrap();
    let superblock = unsafe { std::ptr::read(sb_block.as_ptr() as *const Superblock) };

    println!("--rustfsck: checking {}--", path);

    if superblock.magic != MAGIC {
        println!("FATAL: invalid magic number. Aborting");
        std::process::exit(1);
    }

    println!("Magic Number Good");

    // if std::env::args().nth(2).as_deref() == Some("--corrupt-test") {
    //     let mut d = disk_arc.lock().unwrap();
    //     let data = d.read_block(superblock.inode_bitmap_block).unwrap();
    //     let mut bitmap = Bitmap::from_block(data);
    //     let mut raw = bitmap.to_block();
    //     raw[6] |= 1 << 2;
    //     d.write_block(superblock.inode_bitmap_block, &raw).unwrap();
    //     d.flush_all();
    //     println!("Injected corruption: inode 50 marked used with no directory entry");
    //     return;
    // }

    let mut dir_store = DirStore::new(
        disk_arc.clone(),
        InodeStore::new(disk_arc.clone(), superblock.inode_table_block),
    );

    let mut inode_store = InodeStore::new(disk_arc.clone(), superblock.inode_table_block);

    let mut refrenced_inodes = HashSet::new();
    let mut link_counts = HashMap::new();
    let mut block_owner = HashMap::new();
    let mut visited_dirs = HashSet::new();
    let mut subdir_counts = HashMap::new();
    let mut collected_inodes = HashSet::new();

    refrenced_inodes.insert(ROOT_INODE as u64);
    let mut errors = 0;

    walk_directory(
        ROOT_INODE as u64,
        ROOT_INODE as u64,
        &mut dir_store,
        &mut inode_store,
        &disk_arc,
        &mut refrenced_inodes,
        &mut link_counts,
        &mut subdir_counts,
        &mut block_owner,
        &mut visited_dirs,
        &mut collected_inodes,
        superblock.data_start_block,
        superblock.total_blocks,
        &mut errors,
    );

    println!("\n=== Checking duplicate block allocation ===");

    for (block, owners) in &block_owner {
        if owners.len() > 1 {
            println!(
                "DUPLICATE BLOCK {} refrenced by {} inodes {:?}",
                block,
                owners.len(),
                owners
            );
            errors += 1;
        }
    }

    println!("\n=== checking inode bitmap consitency ===");

    let inode_bitmap_data = disk_arc
        .lock()
        .unwrap()
        .read_block(superblock.inode_bitmap_block)
        .unwrap();
    let inode_bitmap = Bitmap::from_block(inode_bitmap_data);

    for inode_num in 0..superblock.total_inodes {
        let bitmap_says_used = inode_bitmap.is_used(inode_num);
        let actually_referenced = refrenced_inodes.contains(&inode_num);

        if !bitmap_says_used && actually_referenced {
            println!(
                "CORRUPTION: inode {} is refrenced by a directory but bitmap says FREE",
                inode_num
            );
            errors += 1;
        }

        if bitmap_says_used
            && !actually_referenced
            && inode_num != ROOT_INODE as u64
            && inode_num >= 3
        {
            println!(
                "ORPHANED INODE: {} marked used in bitmap but not reachable from root",
                inode_num
            );
            errors += 1;
        }
    }

    println!("\n === checking data bimtap consistency");

    let data_bitmap_data = disk_arc
        .lock()
        .unwrap()
        .read_block(superblock.data_bitmap_block)
        .unwrap();

    let data_bitmap = Bitmap::from_block(data_bitmap_data);

    let total_data_blocks = superblock.total_blocks - superblock.data_start_block;

    for i in 0..total_data_blocks {
        let actual_block = superblock.data_start_block + i;
        let bitmap_used = data_bitmap.is_used(i);
        let actually_used = block_owner.contains_key(&actual_block);

        if bitmap_used && !actually_used {
            println!(
                "LEAKED BLOCK: {} marked used in bitmap but unreachable",
                actual_block
            );
            errors += 1;
        }

        if !bitmap_used && actually_used {
            println!(
                "CORRUPTION: block {} refrenced but bimtap says FREE",
                actual_block
            );
            errors += 1;
        }
    }

    println!("\n=== Checking link counts ===");

    let root_inode = inode_store.read_inode(ROOT_INODE as u64);
    let root_expected = 2 + subdir_counts
        .get(&(ROOT_INODE as u64))
        .copied()
        .unwrap_or(0);
    if root_inode.hard_links != root_expected {
        println!(
            "LINK COUNT MISMATCH: root inode has hard_links = {} but expected {}",
            root_inode.hard_links, root_expected
        );
        errors += 1;
    }

    for (&inode_num, &actual_count) in &link_counts {
        let inode = inode_store.read_inode(inode_num);
        let is_dir = inode.mode & 0o170000 == 0o040000;

        let expected = if is_dir {
            2 + subdir_counts.get(&inode_num).copied().unwrap_or(0)
        } else {
            actual_count
        };

        if inode.hard_links != expected {
            println!(
                "LINK COUNT MISMATCH: inode {} has hard_links= {} but expected {}",
                inode_num, inode.hard_links, expected
            );
            errors += 1;
        }
    }

    println!("\n=== Checking superblock ===");

    let mut actual_used_blocks_bitmap = 0u64;
    for i in 0..total_data_blocks {
        if data_bitmap.is_used(i) {
            actual_used_blocks_bitmap += 1;
        }
    }

    let actual_free_blocks = total_data_blocks - actual_used_blocks_bitmap;

    if actual_free_blocks != superblock.free_blocks {
        println!(
            "SUPERBLOCK ERROR: free_blocks={}, expected {}",
            superblock.free_blocks, actual_free_blocks
        );
        errors += 1;
    }

    let mut actual_used_inodes_bitmap = 0u64;
    for i in 0..superblock.total_inodes {
        if inode_bitmap.is_used(i) {
            actual_used_inodes_bitmap += 1;
        }
    }

    let actual_free_inodes = superblock.total_inodes - actual_used_inodes_bitmap;

    if actual_free_inodes != superblock.free_inodes {
        println!(
            "SUPERBLOCK ERROR: free_inodes={}, expected {}",
            superblock.free_inodes, actual_free_inodes
        );
        errors += 1;
    }

    println!("\n=== Summary ===");

    if errors == 0 {
        println!("FileSystem is CONSISTENT. No errors found.");
    } else {
        println!("Found {} inconcitencies", errors);
    }
}
