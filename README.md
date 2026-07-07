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
├── fuse-daemon/
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
| 3 – 130 | Inode table (128 blocks × 16 inodes/block = 2048 inodes) |
| 131 – 138 | Journal (1 header block + up to 4 data blocks) |
| 139 + | Data blocks |

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
bitmap.alloc()          // first-fit allocation, returns index
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
cargo run --bin fuse-daemon -- myfs.img /tmp/mnt

# check consistency
cargo run --bin fsck -- myfs.img
```

Requirements: Rust 1.75+, `libfuse3-dev` (Linux) or `macfuse` (macOS).

---

## Design Notes

**Why write-back caching?** Batching writes to the same block (e.g., repeated inode table updates) avoids redundant I/O. The journal ensures durability: all metadata writes are journaled before being applied, so a crash between the two leaves a replayable record.

**Why `unsafe` pointer casts?** The on-disk format is a flat C struct. `std::ptr::read` / `std::ptr::write` on `#[repr(C)]` types is the zero-copy way to (de)serialize them without pulling in a serde dependency.

**Root inode is 2**, matching ext2 convention. Inodes 0 and 1 are reserved (0 = null, 1 = bad-block inode).