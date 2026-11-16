// In-process microbenchmark: directory lookup latency vs entry count.
//
// dir.rs walks a linked list of entries in a single 4096-byte block, so
// look_up() is O(n) in the number of entries. This measures that directly,
// bypassing FUSE/kernel dispatch entirely so the O(n) signal isn't swamped
// by round-trip overhead (see bench_metadata_results discussion).
//
// Run: cargo run --release -p libfs --example bench_dir_lookup

use std::sync::{Arc, Mutex};
use std::time::Instant;

use libfs::dir::DirStore;
use libfs::disk::Disk;
use libfs::inode::InodeStore;
use libfs::layout::{BLOCK_SIZE, FT_REGULAR, Inode};

const DIR_INODE_NUM: u64 = 5;
const DIR_DATA_BLOCK: u64 = 1;
const ITERS_PER_RUN: u32 = 100_000;
const RUNS: u32 = 5;
const ENTRY_COUNTS: &[usize] = &[10, 50, 100, 150, 200, 250, 300];

fn setup(path: &str) -> (Arc<Mutex<Disk>>, DirStore) {
    let disk = Arc::new(Mutex::new(Disk::create(path, 4 * BLOCK_SIZE).unwrap()));

    // fake inode table at block 0, holding one directory inode
    let mut setup_inode_store = InodeStore::new(disk.clone(), 0);
    let dir_inode = Inode {
        mode: 0o040755,
        uid: 0,
        gid: 0,
        size: BLOCK_SIZE,
        ctime: 0,
        atime: 0,
        mtime: 0,
        hard_links: 2,
        direct: [DIR_DATA_BLOCK, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        indirect: 0,
        double_indirect: 0,
        _padding: [0u8; 96],
    };
    setup_inode_store.write_inode(DIR_INODE_NUM, &dir_inode);

    // seed the dir data block with a single empty slot spanning the whole
    // block, matching the invariant add_entry()/look_up() expect
    use libfs::layout::DirEntryHeader;
    let mut block = [0u8; BLOCK_SIZE as usize];
    let empty_header = DirEntryHeader {
        inode: 0,
        rec_len: BLOCK_SIZE as u16,
        name_len: 0,
        file_type: 0,
    };
    unsafe {
        std::ptr::write(block.as_mut_ptr() as *mut DirEntryHeader, empty_header);
    }
    disk.lock().unwrap().write_block(DIR_DATA_BLOCK, &block).unwrap();

    let inode_store = InodeStore::new(disk.clone(), 0);
    let dir_store = DirStore::new(disk.clone(), inode_store);
    (disk, dir_store)
}

fn time_lookups(dir_store: &mut DirStore, name: &str, iters: u32) -> f64 {
    let start = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(dir_store.look_up(DIR_INODE_NUM, name.to_string()));
    }
    start.elapsed().as_nanos() as f64 / iters as f64
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

fn stddev(v: &[f64], m: f64) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64;
    var.sqrt()
}

fn main() {
    println!("entries,hit_mean_ns,hit_stddev_ns,miss_mean_ns,miss_stddev_ns");

    for &n in ENTRY_COUNTS {
        let path = format!("/tmp/rustfs-dirbench-{n}.img");
        let (_disk, mut dir_store) = setup(&path);

        for i in 0..n {
            dir_store
                .add_entry(DIR_INODE_NUM, &format!("f{i}"), 100 + i as u32, FT_REGULAR)
                .expect("ENTRY_COUNTS must fit in one directory block");
        }

        let last_name = format!("f{}", n - 1);
        let miss_name = "does_not_exist".to_string();

        let mut hit_runs = Vec::with_capacity(RUNS as usize);
        let mut miss_runs = Vec::with_capacity(RUNS as usize);
        for _ in 0..RUNS {
            hit_runs.push(time_lookups(&mut dir_store, &last_name, ITERS_PER_RUN));
            miss_runs.push(time_lookups(&mut dir_store, &miss_name, ITERS_PER_RUN));
        }

        let hm = mean(&hit_runs);
        let hs = stddev(&hit_runs, hm);
        let mm = mean(&miss_runs);
        let ms = stddev(&miss_runs, mm);

        println!("{n},{hm:.2},{hs:.2},{mm:.2},{ms:.2}");

        let _ = std::fs::remove_file(&path);
    }
}
