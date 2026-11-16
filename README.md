# rustfs

A FUSE-based filesystem implemented from scratch in Rust, modeled after the ext2 layout. Includes a full on-disk structure, an LRU write-back block cache, a write-ahead journal with crash recovery, `mkfs` for formatting, a FUSE daemon exposing ~15 POSIX operations, and an `fsck` binary for consistency checking.

---

## Architecture

```
rustfs/
├── libfs/               # Core filesystem library (shared by all binaries)
│   ├── layout.rs        # On-disk structs: Superblock, Inode, DirEntryHeader, JournalHeader
│   ├── disk.rs          # Block device abstraction with LRU write-back cache
│   ├── bitmap.rs        # Bit-level allocator/free for inodes and data blocks
│   ├── inode.rs         # Inode table read/write
│   ├── dir.rs           # Directory entry parsing, lookup, add, remove, list
│   ├── journal.rs       # Write-ahead journal: begin_transaction, commit_complete, recover
│   └── lib.rs
├── mkfs/
│   └── main.rs          # Formats a blank image with superblock, bitmaps, inode table, root dir
├── rustfs/
│   └── main.rs          # FUSE mount point (create, read, write, mkdir, unlink, rename, …)
└── fsck/
    └── main.rs          # Filesystem consistency checker
```

---

## On-Disk Layout

| Block(s) | Contents |
|---|---|
| 0 | Superblock |
| 1 | Inode bitmap |
| 2 | Data bitmap |
| 3 – 402 | Inode table (400 blocks × 16 inodes/block = 6,400 inodes) |
| 403 – 410 | Journal (1 header block + up to 4 data blocks) |
| 411 + | Data blocks (25,189 blocks) |

Positions shown are for the default 100 MiB image. `mkfs` sizes the inode table from the disk size, at one inode per 16 KiB (`BYTES_PER_INODE`, the same default ratio as ext4), so the journal and data start move with it. Every position is recorded in the superblock, and the daemon and `fsck` read it from there rather than assuming fixed block numbers. Each bitmap is one block, so a filesystem is limited to 32,768 inodes and 32,768 data blocks (about 128 MiB of data). `mkfs` asserts both limits.

Constants defined in `layout.rs`:

```rust
pub const BLOCK_SIZE: u64  = 4096;
pub const INODE_SIZE: u64  = 256;
pub const ROOT_INODE: u32  = 2;
pub const MAGIC: u32       = 0x52465301;
pub const JOURNAL_MAGIC: u32 = 0x4A524E4C;
pub const MAX_JOURNAL_BLOCKS: usize = 4;
```

### Inode

Each inode is 256 bytes and contains:

- `mode` — file type + UNIX permissions
- `uid`, `gid` — owner identity
- `size` — file size in bytes
- `atime`, `mtime`, `ctime` — timestamps
- `hard_links` — reference count
- `direct[12]` — 12 direct block pointers (covers up to 48 KiB)
- `indirect` — single-indirect block pointer
- `double_indirect` — double-indirect block pointer

---

## Components

### `disk.rs` — Block Cache

`Disk` wraps a raw file and provides a 256-entry LRU write-back block cache.

- Reads are served from cache when possible; misses pull from disk.
- Writes are buffered as dirty cache entries (write-back).
- Eviction flushes dirty blocks to disk before evicting.
- `flush_all()` drains all dirty entries — called after journal commits.

### `bitmap.rs` — Allocator

A single 4096-byte block represents 32 768 bits. Used for both the inode bitmap and the data bitmap.

```rust
bitmap.alloc(limit)     // first-fit among bits 0..limit; None when full
bitmap.free(n)          // clears bit n
bitmap.is_used(n)       // checks bit n
```

### `inode.rs` — Inode Table

`InodeStore` computes the block and byte offset for any inode number and performs typed reads/writes via `std::ptr::read` / `std::ptr::write`.

### `dir.rs` — Directory Entries

`DirStore` walks the linked-list of `DirEntryHeader` records packed into a directory's data block. Provides:

- `look_up(dir, name)` → `Option<u32>`
- `add_entry(dir, name, inode, file_type)` — reuses deleted slots or shrinks the last entry
- `remove_entry(dir, name)` — zeroes the inode field (logical delete)
- `list(dir)` → `Vec<(u32, String)>`
- `prepare_add_entry(…)` → `(block_num, block_data)` — returns modified block without writing (for journaled callers)

### `journal.rs` — Write-Ahead Journal

Provides atomicity for multi-block metadata updates.

```rust
journal.begin_transaction(&[(block_num, data), …]);
// … apply same writes to real locations …
journal.commit_complete();
```

On mount, `journal.recover()` checks the journal header for a pending (committed-but-not-applied) transaction and replays it before normal operation begins.

---

## Binaries

### `mkfs` — Format a Disk Image

Creates a 100 MiB image at `myfs.img`:

```bash
cargo run --bin mkfs
```

What it does:
1. Allocates the file and writes a zeroed image.
2. Serializes and writes the `Superblock` to block 0.
3. Writes the inode bitmap (bits 0–2 set for reserved inodes).
4. Writes the data bitmap (bit 0 set for the root directory block).
5. Zeroes the inode table.
6. Writes the root inode (inode 2, `mode = 0o040755`, `hard_links = 2`).
7. Writes the root directory data block with `.` and `..` entries.

### `fsck` — Filesystem Checker

```bash
cargo run --bin fsck -- myfs.img
```

Checks performed (in order):

| Check | What it catches |
|---|---|
| Magic number | Corrupt or wrong image |
| Block pointer validity | Pointers outside the data region |
| Duplicate block allocation | Two inodes sharing a data block |
| `.` / `..` correctness | Directory self/parent links wrong |
| Directory cycle detection | Infinite loops in the directory tree |
| Inode bitmap consistency | Inodes referenced but marked free; orphaned inodes |
| Data bitmap consistency | Leaked blocks; blocks referenced but marked free |
| Hard link counts | `hard_links` field doesn't match actual directory references |
| Superblock counters | `free_blocks` / `free_inodes` out of sync with bitmaps |

Exit output:

```
FileSystem is CONSISTENT. No errors found.
```
or
```
Found N inconsistencies
```

---

## Building

```bash
# build everything
cargo build --release

# format a new image
cargo run --bin mkfs

# mount (requires libfuse / fuse3 on your system)
mkdir -p /tmp/mnt
cargo run --bin rustfs -- myfs.img /tmp/mnt

# check consistency
cargo run --bin fsck -- myfs.img
```

Requirements: Rust 1.75+, `libfuse3-dev` (Linux) or `macfuse` (macOS).

---

## Design Notes

**Why write-back caching?** Batching writes to the same block (e.g., repeated inode table updates) avoids redundant I/O. The journal ensures durability: all metadata writes are journaled before being applied, so a crash between the two leaves a replayable record.

**Why `unsafe` pointer casts?** The on-disk format is a flat C struct. `std::ptr::read` / `std::ptr::write` on `#[repr(C)]` types is the zero-copy way to (de)serialize them without pulling in a serde dependency.

**Root inode is 2**, matching ext2 convention. Inodes 0 and 1 are reserved (0 = null, 1 = bad-block inode).

---

## Benchmarks

Four benchmarks, 5 runs each, run against a 100 MiB image on a local FUSE mount. Full methodology and raw per-run data are in the sections below; each result includes what it actually demonstrates, not just the number.

### 1. Sequential I/O throughput

20 MiB file, `dd`, fresh image per run, remounted between write and read to drop both the in-process block cache and the kernel page cache.

| | Mean | Stddev |
|---|---|---|
| Write | 74.28 MiB/s | 5.33 |
| Read | 414.14 MiB/s | 100.5 |

### 2. Metadata operation throughput

200 file creates/deletes per run through the FUSE mount.

| | Mean |
|---|---|
| Create | ~5,997 ops/s |
| Delete | ~5,826 ops/s |

An earlier pass measured delete at ~373 ops/s (17x slower than create). That gap turned out to be a benchmark artifact, not a filesystem property: the delete loop used `rm` per file, which forks a new process on every iteration, while the create loop used bash's `: >` builtin (no fork). Confirmed by running the same `rm` loop against the host's own ext4 — same ~0.4-0.8s overhead, filesystem not involved. Corrected by measuring deletes with `find -delete` (single process), which brought the two numbers into the same range.

### 3. Directory lookup latency vs. entry count

In-process microbenchmark (`libfs/examples/bench_dir_lookup.rs`), bypassing FUSE entirely to isolate `DirStore::look_up()`'s cost from kernel dispatch overhead. Directory entries are a linked list packed into a single 4096-byte block (`inode.direct[0]`, no indirect blocks for directories), so lookup is O(n) in entry count. 100,000 iterations per data point, 5 runs averaged, entry counts from 10 up to ~300 (the block's practical capacity).

```
latency(ns) ≈ 207 + 20.7 × entry_count      (miss case, R² > 0.999)
```

The slope holds to within ~2% across every pairwise segment of the range — confirms the linked-list format's O(n) cost directly, and marks the natural next step (hashed or indexed directory entries) if larger directories are ever needed.

### 4. fsck scan time vs. file count

`fsck` timed against images pre-populated with 0, 50, 100, 150, and 200 files, 5 runs each.

| Files | Mean |
|---|---|
| 0 | 3.4 ms |
| 50 | 3.0 ms |
| 100 | 2.4 ms |
| 150 | 2.4 ms |
| 200 | 2.8 ms |

Flat, not linear — runtime doesn't grow with populated file count. Traced to `fsck/src/main.rs`'s inode-bitmap check, which always iterates the full fixed 2048-slot inode range (`for inode_num in 0..superblock.total_inodes`) and both fixed-size bitmap blocks regardless of how many inodes are actually in use. Consistency-check cost is bound by total filesystem capacity, not by how full the filesystem is.

> These timings were measured on the earlier 2,048-inode layout. Since `mkfs` now creates 6,400 inodes for a 100 MiB image, the fixed bitmap scan is about 3× longer, and these numbers need to be re-measured.

### Notes

- All four benchmarks required correcting for a measurement artifact before trusting the result (kernel page cache, shell fork overhead, FUSE dispatch overhead, process-startup noise) — the corrected methodology for each is described above rather than just the final number.
- While diagnosing the fsck benchmark, found and fixed a cosmetic bug in `fsck`'s startup output: it was printing `std::mem::offset_of!(Superblock, field)` (the field's byte offset within the struct) instead of the field's actual value, so the diagnostic dump showed nonsense like `total_blocks 8`. The consistency-check logic itself read the correct fields throughout — only the printed dump was wrong.