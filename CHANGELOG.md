# Changelog

Every change to the code is recorded here. Each entry says what was wrong, why it happened, what changed, and how it was verified.

---

## Unreleased

### Changed: mkfs sizes the inode table from the disk size (was a fixed 2,048 inodes)

**Date:** 2026-11-16
**Files:** `mkfs/src/main.rs`, `README.md`

#### Why this was needed

`mkfs` always created a 128-block inode table, which holds **2,048 inodes**, whatever the disk size. On the 100 MiB image, that's one inode for every ~50 KiB of data space (25,461 data blocks ÷ 2,048 inodes ≈ 12 blocks each).

Each file or directory uses one inode, and a small file uses one 4 KiB data block. So with files under ~50 KiB, inodes ran out first:

```
2,045 usable inodes × 1 block each ≈ 8 MiB of data written
→ create/mkdir return ENOSPC ("No space left on device")
→ ≈ 91 MiB of the 99.5 MiB data region (~92%) can never be used
```

`df` would show the disk as mostly free, but no new file could be created. That's wasted capacity, and it's caused by the layout, not by the data.

#### What exactly changed

**1. `mkfs/src/main.rs` line 4: new constant**

```rust
const BYTES_PER_INODE: u64 = 16 * 1024;
```

This means one inode per 16 KiB of disk. 16 KiB is ext4's default ratio (`inode_ratio` in `mke2fs.conf`), and it's a common middle ground between running out of inodes and wasting space on unused inodes.

**2. `mkfs/src/main.rs` lines 22–27: inode table size is computed instead of fixed**

Before:
```rust
let inode_table_blocks: u64 = 128;
...
let total_inodes: u64 = inode_table_blocks * (BLOCK_SIZE / INODE_SIZE);
```

After:
```rust
let inodes_per_block: u64 = BLOCK_SIZE / INODE_SIZE;                                    // 16
let inode_table_blocks: u64 = (size_bytes / BYTES_PER_INODE).div_ceil(inodes_per_block); // 400
let total_inodes: u64 = inode_table_blocks * inodes_per_block;                          // 6,400
```

`div_ceil` rounds up to a whole number of inode-table blocks, so every inode slot in the last table block can be used. `total_inodes` is computed after rounding, so the superblock count always matches the table that was actually allocated.

**3. `mkfs/src/main.rs` lines 29–32: journal and data region follow the table**

These lines weren't edited. They were already computed from `inode_table_blocks`, so they move automatically:

| Region | Before | After |
|---|---|---|
| Inode table | blocks 3–130 (128 blocks) | blocks 3–402 (400 blocks) |
| Journal | blocks 131–138 | blocks 403–410 |
| Data | blocks 139–25,599 (25,461) | blocks 411–25,599 (25,189) |
| Root directory block | 139 | 411 |

**4. `mkfs/src/main.rs` lines 34–47: two new safety checks**

```rust
let bitmap_capacity: u64 = BLOCK_SIZE * 8;   // 32,768 bits
assert!(total_inodes <= bitmap_capacity, ...);
assert!(total_data_blocks <= bitmap_capacity, ...);
```

*Why:* each bitmap is one block, so it can track at most 32,768 objects. Now that the inode count grows with disk size, a larger image (above 512 MiB at 16 KiB per inode, or above ~128 MiB of data blocks) would produce a filesystem whose bitmaps can't describe it. `mkfs` now refuses to create that filesystem instead of silently making a broken one. It panics with a message saying which limit was exceeded.

**5. `mkfs/src/main.rs` lines 49–59: layout is printed at format time**

```
Layout: inode table 3-402 (6400 inodes), journal 403-410, data 411-25599 (25189 blocks)
```

*Why:* the positions are no longer constants, so this shows the actual layout without opening the image.

**6. `mkfs/src/main.rs` line 68: comment corrected**

Changed `//-1 for root inode` to `//-3 for inodes 0, 1 (reserved) and 2 (root)`. The code already subtracted 3. Only the comment was wrong.

**7. `README.md`, "On-Disk Layout" table:** updated to the new block ranges, with a note that positions depend on disk size and are read from the superblock, plus the 32,768-object bitmap limit.

**8. `README.md`, benchmark 4 (fsck scan time):** added a note that those timings were measured on the old 2,048-inode layout and need re-measuring. fsck's inode-bitmap check loops over `0..total_inodes`, so it now scans 6,400 slots instead of 2,048.

#### Effect

| | Before | After |
|---|---|---|
| Total inodes | 2,048 | 6,400 |
| Usable for files/dirs (minus 0, 1, root) | 2,045 | 6,397 (3.1×) |
| Data blocks | 25,461 | 25,189 (−272 blocks ≈ −1.06 MiB, ~1.1% of data) |
| Average file size needed to use the whole disk | ~50 KiB | ~16 KiB |
| Metadata share of disk (blocks before data) | 0.54% | 1.6% |

#### Why nothing else needed to change

The daemon and fsck never hardcode block numbers. They read every position from the superblock:

- `rustfs/src/main.rs` uses `superblock.inode_table_block`, `.journal_start_block` and `.data_start_block`. The allocator limit from the previous entry uses `superblock.total_inodes`, so the new limit of 6,400 applies automatically.
- `Journal::new(..., superblock.journal_start_block)` finds the journal at block 403.
- `fsck` loops over `superblock.total_inodes` and uses `superblock.data_start_block`.
- Inode 2 (root) is still at block 3, byte 512, because the inode table still starts at block 3.

I checked this with a search: the only place the old values (`128`, `131`, `139`) appeared in Rust code was `mkfs`.

#### Compatibility

- **Existing images** formatted with the old `mkfs` (2,048 inodes) still mount and pass fsck, because each image's superblock describes its own layout.
- **New images** need a fresh `cargo run --bin mkfs`. There's no in-place conversion.

#### Verification status

**Not yet compiled or run.** No Rust toolchain was available on the machine where this change was made. `u64::div_ceil` needs Rust 1.73 or later, which the README's requirement (1.75+) and the 2024 edition (1.85+) already cover. To verify on Linux or WSL:

```bash
cargo run --bin mkfs           # expect: "Layout: inode table 3-402 (6400 inodes), journal 403-410, data 411-25599 (25189 blocks)"
cargo run --bin fsck -- myfs.img   # expect: free_inodes 6397, free_blocks 25188, "CONSISTENT"
```

#### Not addressed here

- The ratio is a compile-time constant. A command-line flag, like `mkfs.ext4 -i`, would need `mkfs` to start parsing arguments (it currently takes none).
- Scaling past one-block bitmaps (32,768 inodes or data blocks) would need multi-block bitmaps or ext2-style block groups.

---

### Fixed: allocator handed out inodes and data blocks that don't exist

**Date:** 2026-11-16
**Files:** `libfs/src/bitmap.rs`, `rustfs/src/main.rs`, `README.md`

#### The problem

A bitmap occupies one whole block, which is 4096 bytes or **32,768 bits**. The filesystem tracks fewer objects than that:

| Bitmap | Bits in block | Real objects | Source |
|---|---|---|---|
| Inode bitmap (block 1) | 32,768 | **2,048** inodes | `superblock.total_inodes` (128 table blocks × 16) |
| Data bitmap (block 2) | 32,768 | **25,461** data blocks | `total_blocks − data_start_block` (25,600 − 139) |

`Bitmap::alloc()` searched all 32,768 bits. When every real object was in use, it kept going and returned the first bit past the end, such as inode **2048**. That bit really is 0, but only because no inode 2048 exists to ever use it. The `None` branch in each caller, which replies `ENOSPC` ("No space left on device"), was effectively never reached.

#### What happened as a result

**The 2,049th inode** (from `create`, `mkdir` or `symlink`):

- Inode 2048 maps to block `3 + 2048/16 = 131`, which is the **journal header**, not the inode table (blocks 3–130).
- Before that write, `free_inodes -= 1` runs while `free_inodes` is already 0:
  - **Debug build** (`cargo run`): integer overflow panics and the FUSE daemon crashes. Dirty blocks still in the cache are lost.
  - **Release build**: the value wraps to `u64::MAX` and the code keeps going. The inode is written into block 131, then `commit_complete()` zeroes block 131. The file appears in `ls`, but `stat` returns `ENOENT`, and the superblock's free-inode count is corrupt.

**Data block 25,461 and beyond** (from `write`):

- `alloc_data_block` returned block numbers at or above 25,600, which is past the end of the 100 MiB image. A write there grows the image file. A later read past the old end fails `read_exact`, and the `unwrap()` panics.

#### Root cause

The allocator didn't know how many objects the bitmap actually tracks. That count is stored in the superblock, but nothing passed it to `alloc()`.

#### The fix

`Bitmap::alloc` now takes a `limit` and only considers bits `0..limit`:

```rust
// before
pub fn alloc(&mut self) -> Option<u64>
// after
pub fn alloc(&mut self, limit: u64) -> Option<u64>
```

- It returns `None` when every bit below `limit` is set.
- It bounds the partial last byte correctly when `limit` isn't a multiple of 8 (25,461 isn't).
- It clamps `limit` to 32,768, so a corrupt superblock can't cause an out-of-range index.

Each caller in `rustfs/src/main.rs` passes the real count:

| Caller | Line (approx.) | Limit passed |
|---|---|---|
| `alloc_data_block` | 85 | `superblock.total_blocks − superblock.data_start_block` |
| `create` | 598 | `superblock.total_inodes` |
| `mkdir` | 816 | `superblock.total_inodes` |
| `symlink` | 1195 | `superblock.total_inodes` |

The existing `None` branches now run, so a full filesystem returns `ENOSPC` (or `EFBIG` from `write`). In `alloc_data_block` the `?` returns before `free_blocks -= 1`, so the counter can no longer underflow either.

The `Bitmap` description in `README.md` was updated to match the new signature.

#### Tests added (`libfs/src/bitmap.rs`)

| Test | Checks |
|---|---|
| `alloc_stops_at_limit` | Inodes 0–2047 are allocated in order, then the 2,049th call returns `None` and bit 2048 stays clear. |
| `alloc_limit_not_multiple_of_8` | With a limit of 25,461, bits 25,456–25,460 are allocated and the call after that returns `None`, even though bits 25,461–25,463 are 0. |
| `free_bit_is_reused` | With inodes 0–2 reserved (as mkfs does), allocation starts at 3, and a freed bit is handed out again (first-fit). |

#### Verification status

**Not yet compiled or run.** No Rust toolchain was available on the machine where this change was made. To verify on Linux or WSL:

```bash
cargo build
cargo test -p libfs
```

#### Known remaining issues (not fixed by this change)

- `mkdir` allocates the inode before the data block. If the data block allocation then fails, the inode stays marked used but is unreachable. fsck reports it as an orphaned inode.
- The `Inode` struct is 264 bytes under `#[repr(C)]`, but inode slots are 256 bytes, so each inode write overlaps the next inode's `mode` and `uid`. Tracked separately.
