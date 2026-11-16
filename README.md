# rustfs

A FUSE-based filesystem implemented from scratch in Rust, modeled after the ext2 layout. It includes a full on-disk structure, an LRU write-back block cache, a write-ahead journal that makes file creation crash-safe, `mkfs` for formatting, a FUSE daemon handling 15 filesystem operations, and an independent `fsck` consistency checker.

The filesystem lives in a regular 100 MiB image file. Once mounted, ordinary programs (`ls`, `cp`, `dd`, editors) use it like any other directory.

> **Versions.** [REPORT.pdf](REPORT.pdf) describes the version that was submitted (report-era code, commit `0c458d0`, documented in commit `2b79617`). This README describes the **current** code. Everything that changed since the report is summarized in [Changes since the report](#changes-since-the-report) and documented in full (symptom, cause, fix, proof) in [CHANGELOG.md](CHANGELOG.md). The report's original benchmark numbers are kept unchanged in [Benchmarks](#benchmarks), next to a re-run on the current code.

---

## Architecture

```
rustfs/
├── libfs/               # Core filesystem library (shared by all binaries)
│   ├── layout.rs        # On-disk structs: Superblock, Inode, DirEntryHeader, JournalHeader
│   ├── disk.rs          # Block device abstraction with LRU write-back cache
│   ├── bitmap.rs        # Bit-level allocator/free for inodes and data blocks (+ unit tests)
│   ├── inode.rs         # Inode table read/write (+ unit tests)
│   ├── dir.rs           # Directory entry parsing, lookup, add, remove, list (+ unit tests)
│   ├── journal.rs       # Write-ahead journal: begin_transaction, commit_complete, recover
│   ├── cache_stats.rs   # Cache hit/miss counters printed at unmount
│   └── lib.rs
├── libfs/examples/
│   └── bench_dir_lookup.rs  # In-process directory lookup benchmark
├── mkfs/
│   └── main.rs          # Formats a blank image with superblock, bitmaps, inode table, root dir
├── rustfs/
│   └── main.rs          # FUSE daemon (create, read, write, mkdir, unlink, rename, …)
├── fsck/
│   └── main.rs          # Filesystem consistency checker
└── scripts/
    └── e2e_test.sh      # End-to-end tests through a real FUSE mount
```

How a request flows:

```
program (ls, dd, vim)
   │ system call
kernel VFS ──► FUSE module ──► /dev/fuse
   │ FUSE request (lookup, create, write, …)
rustfs daemon (rustfs/src/main.rs, via the `fuser` crate)
   │
libfs: dir.rs · inode.rs · bitmap.rs · journal.rs
   │ read_block / write_block
disk.rs: 256-block LRU write-back cache
   │
myfs.img (a regular file acting as the disk)
```

`mkfs` and `fsck` use `libfs` directly on the image file, without the kernel or the daemon.

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

The submitted version used a fixed 2,048 inodes, with the inode table at blocks 3–130, the journal at 131–138 and data from block 139. That layout ran out of inodes after about 8 MiB of small files.

**Other image sizes.** Only blocks 0–2 are fixed; the inode table always starts at block 3, and the journal and data regions follow it:

| Image | Inodes | Inode table | Journal | Data starts | Data blocks |
|---|---|---|---|---|---|
| 10 MiB | 640 | 3–42 | 43–50 | 51 | 2,509 |
| 50 MiB | 3,200 | 3–202 | 203–210 | 211 | 12,589 |
| 100 MiB (default) | 6,400 | 3–402 | 403–410 | 411 | 25,189 |
| 130 MiB (about the maximum) | 8,320 | 3–522 | 523–530 | 531 | 32,749 |

Above about 130 MiB the data region exceeds the 32,768 blocks one bitmap block can track, and `mkfs` stops. The size is currently set in code (`size_mb` in `mkfs/src/main.rs`); there is no command-line option yet.

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

Each inode is exactly 256 bytes (enforced at compile time) and contains:

- `mode`: file type + UNIX permissions
- `uid`, `gid`: owner identity
- `size`: file size in bytes
- `atime`, `mtime`, `ctime`: timestamps
- `hard_links`: reference count
- `direct[12]`: 12 direct block pointers (up to 48 KiB)
- `indirect`: single-indirect block pointer (512 more blocks, +2 MiB)
- `double_indirect`: double-indirect block pointer (512 × 512 more blocks, +1 GiB)

`#[repr(C)]` places every `u64` on an 8-byte boundary, which adds 8 hidden padding bytes (4 after `gid`, 4 after `hard_links`). The explicit `_padding` is therefore 88 bytes, and `const _: () = assert!(size_of::<Inode>() == INODE_SIZE)` stops the build if the size ever drifts. (In the submitted version `_padding` was 96, making the struct 264 bytes; every inode write overwrote the next inode's `mode` and `uid`. See CHANGELOG.)

To find inode *N*: block `inode_table_block + N / 16`, byte offset `(N % 16) × 256`.

---

## Components

### `disk.rs`: Block Cache

`Disk` wraps the image file and provides a 256-entry (1 MiB) LRU write-back block cache.

- Reads are served from cache when possible; misses read from the image file.
- Writes only update the cache and mark the block dirty (write-back).
- Dirty blocks reach the image file when evicted, on `flush_all()` (called at the start of every journal transaction) and at unmount.
- There is no `fsync`: "flushed" means handed to the operating system, not guaranteed to be on the physical disk.

### `bitmap.rs`: Allocator

A single 4096-byte block represents 32,768 bits. Used for both the inode bitmap and the data bitmap.

```rust
bitmap.alloc(limit)     // first-fit among bits 0..limit; None when full
bitmap.free(n)          // clears bit n
bitmap.is_used(n)       // checks bit n
```

`limit` is the real number of inodes or data blocks, so the allocator never hands out an object past the end of the inode table or the image.

### `inode.rs`: Inode Table

`InodeStore` computes the block and byte offset for any inode number and performs typed reads/writes via `std::ptr::read` / `std::ptr::write`.

### `dir.rs`: Directory Entries

A directory's entries are a chain of `DirEntryHeader` records (inode, `rec_len`, `name_len`, file type, then the name) packed into one 4096-byte block. `rec_len` is the distance to the next entry; the last entry's `rec_len` runs to the end of the block. About 341 short names fit in one block. `DirStore` provides:

- `look_up(dir, name)` → `Option<u32>` (linear walk, O(n))
- `prepare_add_entry(dir, name, inode, type)` → `Option<(block_num, block_data)>`: the modified block **without writing it**, so `create` can put it in a journal transaction; `None` if the block is full
- `add_entry(…)` → `Option<()>`: `prepare_add_entry` plus the write; reuses deleted slots or shrinks the last entry
- `has_space_for(dir, name)` → `bool`: a dry run, so handlers can refuse with `ENOSPC` before changing anything
- `remove_entry(dir, name)`: sets the entry's inode to 0 (the slot can be reused)
- `list(dir)` → `Vec<(u32, String)>`

### `journal.rs`: Write-Ahead Journal

Makes a multi-block metadata update all-or-nothing.

```rust
journal.begin_transaction(&[(block_num, data), …]);  // header + copies, then flush_all
// … apply the same writes to their real locations …
journal.commit_complete();                           // clear the header
```

On mount, `journal.recover()` checks the journal header. If it holds a committed transaction, it copies each journaled block to its real location, flushes, and clears the header. Replay writes whole block images, so replaying twice gives the same result.

**Scope:** only `create` uses the journal (inode bitmap, superblock, inode-table block and the parent directory block, 4 blocks). `mkdir`, `unlink`, `rmdir`, `rename`, `symlink`, `link` and `write` update blocks directly, so a crash in the middle of one of them can leave the filesystem inconsistent (`fsck` will report it). Setting `CRASH_TEST=1` makes `create` exit right after writing the journal, to test recovery.

---

## Binaries

### `mkfs`: Format a Disk Image

Creates a 100 MiB image named `myfs.img` in the current directory:

```bash
cargo run --release --bin mkfs
```

What it does:
1. Creates the image file (100 MiB).
2. Works out the layout (inode table sized from the disk size) and checks the bitmap limits.
3. Writes the `Superblock` to block 0.
4. Writes the inode bitmap (inodes 0–2 used: 0 and 1 reserved, 2 is the root).
5. Writes the data bitmap (one block used for the root directory).
6. Zeroes the inode table.
7. Writes the root inode (inode 2, `mode = 0o040755`, `hard_links = 2`).
8. Writes the root directory block with `.` and `..`.

### `rustfs`: The FUSE Daemon

```bash
mkdir -p /tmp/mnt
cargo run --release --bin rustfs -- myfs.img /tmp/mnt
```

Runs in the foreground until the filesystem is unmounted (`fusermount3 -u /tmp/mnt`). Handles `lookup`, `getattr`, `setattr`, `readdir`, `create`, `read`, `write`, `mkdir`, `rmdir`, `unlink`, `rename`, `symlink`, `readlink`, `link` and `destroy`. If the image or the mount point doesn't exist, it prints a clear message and exits with code 1.

Error behaviour worth knowing:
- A full directory or a full disk returns `ENOSPC`, and nothing is changed. Every handler checks for space **before** allocating anything.
- `rename` replaces an existing target as POSIX requires, and refuses file-over-directory (`EISDIR`), directory-over-file (`ENOTDIR`) and directory-over-non-empty-directory (`ENOTEMPTY`).
- Writing past the maximum file size returns `EFBIG`.

### `fsck`: Filesystem Checker

```bash
cargo run --release --bin fsck -- myfs.img
```

Run it on an **unmounted** image. It walks the directory tree from the root and compares what it finds with the bitmaps, link counts and superblock counters.

| Check | What it catches |
|---|---|
| Magic number | Corrupt or wrong image |
| Block pointer validity | Pointers outside the data region |
| Duplicate block allocation | Two inodes sharing a data block |
| `.` / `..` correctness | Directory self/parent links wrong |
| Directory cycle detection | Infinite loops in the directory tree |
| Valid file type | An in-use inode whose `mode` is not a file, directory or symlink |
| Duplicate names | The same name appearing twice in one directory |
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

## Building and Running

```bash
# build everything (release build; use this for benchmarks)
cargo build --release

# format a new image (unmount first if one is mounted!)
cargo run --release --bin mkfs

# mount (requires libfuse / fuse3); stays in the foreground
mkdir -p /tmp/mnt
cargo run --release --bin rustfs -- myfs.img /tmp/mnt

# in another terminal: use it, then unmount
ls /tmp/mnt
fusermount3 -u /tmp/mnt

# check consistency (on the unmounted image)
cargo run --release --bin fsck -- myfs.img
```

Requirements: Rust 1.85+ (the crates use edition 2024), `libfuse3-dev` / `fuse3` (Linux) or macFUSE (macOS).

Things that will bite you:
- **Never run `mkfs` while the daemon is mounted.** When the daemon exits, its write-back cache writes its old blocks into the new image.
- **Keep the image (and ideally the build directory) on a native Linux filesystem** such as ext4. On an NTFS partition mounted through ntfs-3g, writes measured 4–5× slower and every program launch costs extra, because every block goes through two FUSE filesystems.

---

## Testing

```bash
# unit tests: bitmap limits, full directory block, inode size and neighbours
cargo test -p libfs

# end-to-end tests through a real FUSE mount (needs a release build)
cargo build --release
scripts/e2e_test.sh [BIN_DIR] [WORK_DIR]   # defaults: target/release, ~/rustfs-bench
```

`scripts/e2e_test.sh` runs 14 cases (51 checks). Each case formats a fresh image, mounts it, runs real shell commands, **remounts** (to empty the kernel's caches, which can otherwise hide bugs), checks the results, and runs `fsck`. It covers renames (moving directories, replacing files, editor-style saves, hard links, symlinks, type mismatches, full directories, `renameat2` flags), a full disk, write integrity (large, unaligned, cross-block and appending writes compared byte for byte), bad command-line arguments, and `fsck` detecting deliberately damaged images. Keep `WORK_DIR` on ext4.

The suite was also run against the report-era code (built in a `git worktree`) to confirm each test fails on the old code where a bug existed: **old 31/51, current 51/51**.

---

## Changes Since the Report

All dated 2026-11-16; full write-ups in [CHANGELOG.md](CHANGELOG.md).

| Change | Effect in the submitted version | Where |
|---|---|---|
| Allocator bounded by the real object count | Past 2,045 files the next inode landed in the journal header and free counters underflowed | `bitmap.rs`, `rustfs` |
| Inode table sized from the disk (1 per 16 KiB) | Fixed 2,048 inodes ran out after ~8 MiB of small files | `mkfs` |
| Full directory returns `ENOSPC` | Creating the ~341st entry in a directory crashed the daemon (the mount disappeared) | `dir.rs`, `rustfs` |
| Inode struct is exactly 256 bytes | It was 264 bytes; each inode write wiped the next inode's type and owner (a directory became an unreadable root-owned file) | `layout.rs` + inode constructors |
| `rename` of a directory to a new parent | The moved directory looked empty; its contents were orphaned | `rustfs` |
| `rename` onto an existing name | Left two entries with the same name; after a remount the old file came back (editor saves reverted) | `rustfs` |
| `mkdir` on a full disk | Leaked an inode | `rustfs` |
| Full disk error code | Reported "File too large" instead of "No space left on device" | `rustfs` |
| `fsck` file-type and duplicate-name checks | `fsck` misreported or missed the two corruption bugs above | `fsck` |
| Clear errors for a missing image or mount point | The daemon panicked | `rustfs` |
| Same-directory rename in a full directory; `renameat2` flags | `ENOSPC` when a rename would fit; flags ignored | `rustfs` |
| `write()` block-boundary tripwire | Would panic if one request crossed a block (not reachable with current FUSE settings); now returns `EIO` | `rustfs` |
| Unit tests, end-to-end suite | Testing was fsck runs and crash injection only | `libfs`, `scripts/` |

---

## Benchmarks

### Original results (as published in REPORT.pdf)

These are the numbers from the submitted version, kept unchanged.

Four benchmarks, 5 runs each, run against a 100 MiB image on a local FUSE mount. Full methodology and raw per-run data are in the sections below; each result includes what it actually demonstrates, not just the number.

#### 1. Sequential I/O throughput

20 MiB file, `dd`, fresh image per run, remounted between write and read to drop both the in-process block cache and the kernel page cache.

| | Mean | Stddev |
|---|---|---|
| Write | 74.28 MiB/s | 5.33 |
| Read | 414.14 MiB/s | 100.5 |

#### 2. Metadata operation throughput

200 file creates/deletes per run through the FUSE mount.

| | Mean |
|---|---|
| Create | ~5,997 ops/s |
| Delete | ~5,826 ops/s |

An earlier pass measured delete at ~373 ops/s (17x slower than create). That gap turned out to be a benchmark artifact, not a filesystem property: the delete loop used `rm` per file, which forks a new process on every iteration, while the create loop used bash's `: >` builtin (no fork). Confirmed by running the same `rm` loop against the host's own ext4 — same ~0.4-0.8s overhead, filesystem not involved. Corrected by measuring deletes with `find -delete` (single process), which brought the two numbers into the same range.

#### 3. Directory lookup latency vs. entry count

In-process microbenchmark (`libfs/examples/bench_dir_lookup.rs`), bypassing FUSE entirely to isolate `DirStore::look_up()`'s cost from kernel dispatch overhead. Directory entries are a linked list packed into a single 4096-byte block (`inode.direct[0]`, no indirect blocks for directories), so lookup is O(n) in entry count. 100,000 iterations per data point, 5 runs averaged, entry counts from 10 up to ~300 (the block's practical capacity).

```
latency(ns) ≈ 207 + 20.7 × entry_count      (miss case, R² > 0.999)
```

The slope holds to within ~2% across every pairwise segment of the range — confirms the linked-list format's O(n) cost directly, and marks the natural next step (hashed or indexed directory entries) if larger directories are ever needed.

#### 4. fsck scan time vs. file count

`fsck` timed against images pre-populated with 0, 50, 100, 150, and 200 files, 5 runs each.

| Files | Mean |
|---|---|
| 0 | 3.4 ms |
| 50 | 3.0 ms |
| 100 | 2.4 ms |
| 150 | 2.4 ms |
| 200 | 2.8 ms |

Flat, not linear — runtime doesn't grow with populated file count. Traced to `fsck/src/main.rs`'s inode-bitmap check, which always iterates the full fixed 2048-slot inode range (`for inode_num in 0..superblock.total_inodes`) and both fixed-size bitmap blocks regardless of how many inodes are actually in use. Consistency-check cost is bound by total filesystem capacity, not by how full the filesystem is.

#### Notes (original)

- All four benchmarks required correcting for a measurement artifact before trusting the result (kernel page cache, shell fork overhead, FUSE dispatch overhead, process-startup noise) — the corrected methodology for each is described above rather than just the final number.
- While diagnosing the fsck benchmark, found and fixed a cosmetic bug in `fsck`'s startup output: it was printing `std::mem::offset_of!(Superblock, field)` (the field's byte offset within the struct) instead of the field's actual value, so the diagnostic dump showed nonsense like `total_blocks 8`. The consistency-check logic itself read the correct fields throughout — only the printed dump was wrong.

### Re-run after the fixes (2026-11-16)

The same four benchmarks, re-run to check that the fixes cost no performance. The original run didn't record its conditions, so comparing new numbers against the table above alone could blame the code for differences caused by the machine. Instead, the report-era code (`0c458d0`) and the current code were built side by side and **run alternately under identical conditions**:

- release builds, with the binaries **and** the images on ext4 (not the NTFS partition)
- power profile `performance`, on AC power
- old and new alternate every round, and the order flips each round

| Benchmark | Original (report) | Report-era code, re-run | Current code | Verdict |
|---|---|---|---|---|
| Sequential write | 74.28 MiB/s | 72.7 MiB/s | 75.1 MiB/s | Same |
| Sequential read | 414.14 MiB/s | 318 MiB/s (mean 344) | 346 MiB/s (mean 362) | Same; reads take ~50 ms, so they're noisy |
| Create, 200 files | ~5,997 ops/s | 4,675 ops/s | 4,754 ops/s | Same between old and new |
| Delete, 200 files | ~5,826 ops/s | 12,222 ops/s | 13,896 ops/s | Same between old and new |
| Directory lookup, per entry | 20.7 ns | 21.3 ns | 25.3 ns | Code placement, see below |
| fsck, 0–200 files | 2.4–3.4 ms | 2.9–3.7 ms | ≈ same on the same image | Same, see below |

Values are medians unless stated.

**What the differences mean:**

- **Under controlled conditions the report-era code reproduces the original numbers** closely (write 72.7 vs 74.28 MiB/s, lookup 21.3 vs 20.7 ns, fsck 2.9–3.7 vs 2.4–3.4 ms). Earlier attempts that looked slower than the report were caused by the test conditions (binaries on an NTFS partition, the balanced power profile), not by the code.
- **Read, create and delete:** old and new code are within noise of each other. Create is lower than the original on this machine for **both** versions, and delete is higher for both, so those gaps are about the machine state and the original measurement, not the fixes.
- **Directory lookup (+19%) is code placement, not behaviour.** `look_up` has no changed lines, and its compiled instructions are identical in both versions. The inode-size fix made a struct 8 bytes smaller, which moved the hot loop to a different position in memory. Rebuilding both with `RUSTFLAGS="-C llvm-args=-align-loops=64"` (every loop on a 64-byte boundary) gives 20.70 ns (old) vs 21.50 ns (new). The lookup is still a straight line in entry count: 243 + 25.3 × n ns.
- **fsck: the inode count doesn't matter**, which corrects the original explanation above. Going from 2,048 to 6,400 inodes changed nothing measurable (the same fsck binary on both kinds of image: 4.75 vs 4.55 ms and 4.46 vs 4.41 ms). On the same image, old and new fsck take the same time (4.80 vs 4.75 ms). fsck's time is mostly program start-up and block reads, not the inode loop. It stays flat as files are added, as originally measured.

Methodology lessons from the re-run:
- Launching a program from the NTFS partition added about 0.5 ms per run (measured with an identical copy of fsck on ext4). That matters for anything started many times, like fsck.
- A freshly written image can still be being saved to disk by the kernel; timing fsck on it right away produced a false 15–31% "slowdown". Let new files settle (`sync`) before timing.
- This laptop's CPU switches between clock levels, which shows up as two clusters in short measurements (delete, fsck). Use medians and several runs.

Raw per-run data and the full investigation are in `SESSION_NOTES.md`.

---

## Design Notes

**Why write-back caching?** Bitmap, superblock and inode-table blocks are rewritten on almost every operation; caching turns many writes of the same block into one. The cost is that changes live in memory until flushed, which is why the multi-block `create` goes through the journal.

**What the journal does and doesn't guarantee.** `create` is atomic with respect to the daemon crashing: either all four blocks change or none do, and replay at the next mount finishes an interrupted transaction (tested with `CRASH_TEST`). Other operations are not journaled. Because nothing calls `fsync`, a power loss or OS crash can still lose recently written data.

**Why `unsafe` pointer casts?** The on-disk format is a flat C struct. `std::ptr::read` / `std::ptr::write` on `#[repr(C)]` types is the zero-copy way to (de)serialize them without a serialization dependency. Because `repr(C)` adds alignment padding, each struct's size must match its slot exactly; the inode size is checked at compile time.

**Check before changing anything.** Every handler that can fail for lack of space or a type mismatch checks first, so a refused request leaves the disk exactly as it was (no leaked inodes or blocks).

**One request at a time.** The `fuser` session handles requests one by one and all disk access goes through one `Arc<Mutex<Disk>>`, so a check followed by a change in the same handler can't be raced.

**`write()` assumes one block per request.** With the current FUSE settings the kernel sends at most one page-aligned 4 KiB page per write request, so a request never crosses a block. Enabling larger writes, writeback caching or direct I/O would break that; `write()` then returns `EIO` and logs the request instead of panicking, and would need to loop over blocks.

**Root inode is 2**, matching ext2 convention. Inodes 0 and 1 are reserved (0 = null, 1 = bad-block inode). The kernel calls the root inode 1, so handlers map 1 → 2.

---

## Known Limitations

| Limitation | Effect |
|---|---|
| Only `create` is journaled | A crash during other operations can leave inconsistencies (fsck detects them) |
| No `fsync` | Not durable against power loss or an OS crash |
| One-block directories | About 341 short names per directory; more returns `ENOSPC` |
| One-block bitmaps | At most 32,768 inodes and 32,768 data blocks (about 130 MiB images) |
| Image size set in code | `size_mb` in `mkfs/src/main.rs`; no command-line option |
| Single-threaded | One request at a time |
| Directory lookup is O(n) | Linear walk of the entry chain |
| Maximum file size | About 1 GiB (12 direct + single + double indirect), larger than the image anyway |
