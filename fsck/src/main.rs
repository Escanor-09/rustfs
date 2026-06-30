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

fn collect_blocks(inode: &Inode, disk: &Arc<Mutex<Disk>>, refrenced_blocks: &mut HashSet<u64>) {
    const PTRS_PER_BLOCK: usize = (BLOCK_SIZE as usize) / std::mem::size_of::<u64>();

    //Direct Blocks
    for &block in &inode.direct {
        if block != 0 {
            refrenced_blocks.insert(block);
        }
    }

    //Single Indirect
    if inode.indirect != 0 {
        refrenced_blocks.insert(inode.indirect);

        let indirect_block = disk.lock().unwrap().read_block(inode.indirect).unwrap();

        let ptrs = unsafe {
            std::slice::from_raw_parts(indirect_block.as_ptr() as *const u64, PTRS_PER_BLOCK)
        };

        for &block in ptrs {
            if block != 0 {
                refrenced_blocks.insert(block);
            }
        }
    }

    //Double Indirect
    if inode.double_indirect != 0 {
        //outer pointer block
        refrenced_blocks.insert(inode.double_indirect);

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

            //inner indirect block
            refrenced_blocks.insert(inner_block);

            let inner = disk.lock().unwrap().read_block(inner_block).unwrap();

            let inner_ptrs =
                unsafe { std::slice::from_raw_parts(inner.as_ptr() as *const u64, PTRS_PER_BLOCK) };

            for &data_block in inner_ptrs {
                if data_block != 0 {
                    refrenced_blocks.insert(data_block);
                }
            }
        }
    }
}

fn walk_directory(
    dir_inode_num: u64,
    dir_store: &mut DirStore,
    inode_store: &mut InodeStore,
    disk: &Arc<Mutex<Disk>>,
    refrenced_inodes: &mut HashSet<u64>,
    link_counts: &mut HashMap<u64, u32>,
    subdir_counts: &mut HashMap<u64, u32>,
    refrenced_blocks: &mut HashSet<u64>,
    visited_dirs: &mut HashSet<u64>,
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
    collect_blocks(&current_inode, disk, refrenced_blocks);

    let entries = dir_store.list(dir_inode_num);

    for (inode_num, name) in entries {
        let inode_num = inode_num as u64;

        if name == "." || name == ".." {
            continue;
        }

        refrenced_inodes.insert(inode_num);
        *link_counts.entry(inode_num).or_insert(0) += 1;

        let inode = inode_store.read_inode(inode_num);

        collect_blocks(&inode, disk, refrenced_blocks);

        let is_dir = inode.mode & 0o170000 == 0o040000;
        if is_dir {
            *subdir_counts.entry(dir_inode_num).or_insert(0) += 1;
            walk_directory(
                inode_num,
                dir_store,
                inode_store,
                disk,
                refrenced_inodes,
                link_counts,
                subdir_counts,
                refrenced_blocks,
                visited_dirs,
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
    let mut refrenced_blocks = HashSet::new();
    let mut visited_dirs = HashSet::new();
    let mut subdir_counts = HashMap::new();

    refrenced_inodes.insert(ROOT_INODE as u64);

    walk_directory(
        ROOT_INODE as u64,
        &mut dir_store,
        &mut inode_store,
        &disk_arc,
        &mut refrenced_inodes,
        &mut link_counts,
        &mut subdir_counts,
        &mut refrenced_blocks,
        &mut visited_dirs,
    );

    println!("\n=== checking inode bitmap consitency ===");
    let mut errors = 0;

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

    println!("\n=== Summary ===");

    if errors == 0 {
        println!("FileSystem is CONSISTENT. No errors found.");
    } else {
        println!("Found {} inconcitencies", errors);
    }
}
