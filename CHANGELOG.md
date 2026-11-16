# Changelog

Every change to the code is recorded here. Each entry says what was wrong, why it happened, what changed, and how it was verified.

---

## Unreleased

### Fixed: small bugs left after the rename fix (mkdir leak, wrong error on full disk, fsck blind spots, panics on bad arguments, rename edge cases)

**Date:** 2026-11-16
**Files:** `rustfs/src/main.rs`, `fsck/src/main.rs`, `scripts/e2e_test.sh`
**Status:** Verified (end-to-end suite: new code 51/51, old code 31/51)

Each fix is small and local. Design changes were deliberately left out (see "Not done" at the end).

#### 1. `mkdir` leaked an inode when the disk was full

**Found by:** filling the disk (`dd … bs=1M count=200`), then `mkdir newdir` → `ENOSPC` (correct), but fsck reported `ORPHANED INODE: 4 marked used in bitmap but not reachable from root`, and `free_inodes` was 6395 instead of 6396.

**Why:** `mkdir` takes an inode from the bitmap (and writes the bitmap and superblock), and only **then** allocates the folder's data block. When the block allocation failed, it returned `ENOSPC` without giving the inode back.

**Fix (2 lines in the failure branch):** free the inode with the existing `free_inode` helper before returning. The inode passed in is all zeros (size 0), so there are no data blocks to free; it only clears the bitmap bit and restores `free_inodes`.

**Why not reorder (block first, then inode)?** That just moves the leak: the block would leak if the inode allocation failed. Undoing on failure is the smaller, safer change.

#### 2. A full disk reported "File too large" instead of "No space left on device"

**Found by:** the same `dd`: `dd: error writing '/tmp/mnt/fill': File too large` at 98 MiB, on a 100 MiB disk.

**Why:** `write()` returned `EFBIG` whenever `get_or_alloc_block` returned `None`. That function returns `None` for two different reasons: the file would exceed the largest size the inode can address (12 + 512 + 512×512 blocks ≈ 1 GiB), **or** the disk has no free blocks.

**Fix (2 lines):** if the block index is past the largest file size → `EFBIG`; otherwise → `ENOSPC`. The rest of `write()` is unchanged.

**Why it matters:** programs handle these errors differently. `ENOSPC` means "free some space and retry"; `EFBIG` means "this file can't grow". Users were also told the wrong thing.

#### 3. fsck missed two kinds of damage

| New check | Where | What it catches | Before |
|---|---|---|---|
| `INVALID FILE TYPE` | `walk_directory`, for every reachable inode | `mode & 0o170000` is not a regular file, directory or symlink | The inode-size bug zeroed a directory's `mode`; fsck reported only misleading `LINK COUNT MISMATCH` errors |
| `DUPLICATE NAME` | `walk_directory`, a set of names per folder | The same name appears twice in one folder | The rename bug's duplicate entries were reported as **CONSISTENT** |

About 25 lines in total, using the folder walk that already exists.

#### 4. `rustfs` panicked on a missing mount point or image

**Found by:** a mistyped mount (`/tmp/mnt` didn't exist after a reboot): `panicked at rustfs/src/main.rs:507:54: called Result::unwrap() on an Err value … No such file or directory`.

**Fix:** `main()` checks that the image file and the mount point exist, and prints a clear message with exit code 1. The `mount2(...).unwrap()` became an `if let Err(e)` with a message. Examples:
```
rustfs: mount point '/tmp/x' is not a directory (try: mkdir -p /tmp/x)
rustfs: image 'foo.img' not found (create one with mkfs)
```

#### 5. Renaming to a new name in a full folder returned `ENOSPC`

**Why:** `rename` checked for room for the new name **before** removing the old one. In a full folder, renaming within the same folder (`mv f1 g1`) was refused, even though removing `f1` frees the space `g1` needs.

**Fix (same folder only, ~15 lines):** remove the old entry first, then check whether the new name fits. If it does, continue (and skip the later removal). If it doesn't (for example, a much longer name), **put the old entry back** and return `ENOSPC`. The put-back always fits because the old entry's own slot was just freed. For different folders, the behaviour is unchanged (the target folder must have room).

**Note:** this is the one place where `rename` changes something before it's sure it will succeed. The change is undone on the failure path, and the e2e test checks that the old name survives a refused rename.

#### 6. `renameat2` flags were ignored (backup check only)

`rename` now returns `EINVAL` for `RENAME_EXCHANGE` (swapping isn't supported) and `EEXIST` for `RENAME_NOREPLACE` when the target exists.

**Why:** after the rename fix, an unrecognised swap request would have been treated as "replace", which **deletes** the other file.

**Honest note:** on this system these flags never reach the daemon. The kernel already refuses `NOREPLACE` when the target exists, and refuses `EXCHANGE` before calling FUSE, so the old code passes these tests too. The check only protects against a different kernel or FUSE library version.

#### 7. `write()`: a tripwire instead of a hidden assumption

**The assumption:** `write()` copies the whole request into **one** 4 KiB block (`block[off..off + len]`). If a request ever crossed a block boundary, that slice would panic and take the daemon down.

**Why it can't happen now:** this daemon doesn't enable any optional FUSE write features, so the kernel sends at most one page (4 KiB), aligned to page boundaries, per request. Each page lines up with exactly one block. Verified with large, unaligned, cross-block and appending writes (all byte-for-byte correct).

**What would make it happen:** only a change to this project. For example, enabling larger writes (`max_write` / big writes, the obvious next step for write speed), writeback caching, direct I/O, or a `fuser` upgrade that changes defaults. Whoever made that change would get a crash with nothing pointing back here.

**Fix (decided with the user: option 2 of 3):** before allocating anything, check `bytes_offset_in_block + data.len() > BLOCK_SIZE`. If true, log `rustfs: write of N bytes at offset X crosses a block boundary; not supported` and return `EIO` instead of panicking. A comment above the check states the assumption and says the function must become a loop over blocks if those settings change.

- *Option 1 (leave it)* was rejected: a future change would cause a silent crash.
- *Option 3 (loop over blocks, ~20 lines)* is postponed: it's a design change, worth doing only together with enabling larger writes.

**Verification:**
- e2e suite still 51/51, and the tripwire never fired, confirming the assumption holds.
- Write speed (20 MiB `dd`, release, ext4, 5 runs): 76.1, 80.8, 88.3, 96.5, 92.1 MiB/s, mean ≈ 87, within the earlier 90.7 ± 11.0. One comparison per request costs nothing measurable.
- The tripwire **path itself can't be triggered** through the kernel with the current settings, so it's covered by review, not by a test.

#### Verification: `scripts/e2e_test.sh` (6 new cases, 21 new checks)

| Case | Old code (`ead0734`) | New code |
|---|---|---|
| Rename to a new, same-length name in a full folder | `ENOSPC` | ok |
| Rename to a longer name in a full folder | refused, old name kept | refused, old name kept |
| `renameat2` EXCHANGE / NOREPLACE | refused (by the kernel) | refused |
| Disk full: write error | **"File too large"** | "No space left on device" |
| Disk full: mkdir | refused, **fsck: ORPHANED INODE** | refused, fsck CONSISTENT |
| Write integrity (300 KB random, cross-block `pwrite`, 50 unaligned appends) | ok | ok |
| Missing mount point / image | **panic, exit 101** | message, exit 1 |
| fsck on an image with a wiped `mode` | not reported as a type error | `INVALID FILE TYPE: inode 3` |
| fsck on an image with a duplicate name | **CONSISTENT** | `DUPLICATE NAME: 'ab'` |
| **Whole suite** | **31 passed, 20 failed** (includes the rename-fix cases) | **51 passed, 0 failed** |

The fsck cases damage a test image on purpose with a few bytes of Python: zero an inode's `mode`, or rename an entry `ac` → `ab` in the folder block.

`cargo test -p libfs`: 9 passed.

#### Not done (design changes; left for a decision)

- **`write()` handling a request that spans several blocks** (a loop over blocks). It was written and then **rolled back** as a design change. The tripwire (item 7) covers it until larger writes are enabled; do it at the same time as that change.
- **Journaling the remaining operations** (only `create` is journaled).
- **Multi-block directories and bitmaps.**

---

### Fixed: `rename` lost data (moved folders looked empty; renaming onto an existing name left duplicates)

**Date:** 2026-11-16
**Files:** `rustfs/src/main.rs` (only `rename` and a new helper `free_inode`), `scripts/e2e_test.sh` (new)
**Status:** Verified (end-to-end script: 30/30 on the new code, 12 failures on the old code)

Kept deliberately small: `unlink` and `rmdir` are untouched, and `renameat2` flags are still ignored.

#### Background: how a directory block is read

Entries are read in a chain. Each entry header has `rec_len`, the distance to the next entry, and the last entry's `rec_len` reaches the end of the block. `.` is always at byte 0 (12 bytes) and `..` always at byte 12. In a brand-new directory, `..` is the last entry, so its `rec_len` is `4096 - 12`. Adding an entry shrinks `..` to 12 and appends after it.

#### Bug A: moving a directory to a new parent hid everything inside it

**Found by:** `mkdir d1 d2; touch d1/f; mv d1 d2/` → `ls d2/d1` was empty, and fsck reported `ORPHANED INODE: 5`.

**Why:** to point the moved directory's `..` at its new parent, `rename` wrote a **whole new header** at byte 12, copied from `mkdir`:
```rust
DirEntryHeader { inode: new_parent, rec_len: BLOCK_SIZE - 12, name_len: 2, file_type: FT_DIRECTORY }
```
`rec_len = 4096 - 12` is only correct for an empty directory. For one with contents, it made `..` jump to the end of the block, so every entry after it was skipped. The entries were still on disk but unreachable: the files, their inodes and blocks were orphaned and leaked.

**Fix:** read the existing `..` header, change **only** its `inode` field, and write it back (`read_unaligned`/`write_unaligned`, because the block is a byte array). A `debug_assert` checks that the entry at byte 12 really is `..`.

#### Bug B: renaming onto an existing name added a second entry with that name

**Found by:** `echo AAA > a; echo BBB > b; mv a b` → `ls -i` showed `3 b` twice. `cat b` showed `AAA` (from the kernel's cache), but **after a remount it showed `BBB`**: the rename was undone. `rm b` left one `b` behind. fsck said CONSISTENT.

**Why:** `rename` called `add_entry(new_parent, new_name, …)` without checking whether `new_name` already existed. POSIX `rename(2)` says an existing target is **replaced**. Because nothing removed the old entry:
- the directory held two entries with the same name, and `look_up` returns the first one, which is the old one
- the replaced file's link count was never decreased, so its inode and blocks were never freed

**Impact:** editors and tools save by writing a temporary file and renaming it over the original (vim, gedit, git, `sed -i`). On rustfs those saves **reverted to the old content after a remount**. Renaming a folder over a **non-empty** folder was also accepted, and the target's contents were lost.

**Fix:** before changing anything, `rename` looks up `new_name` in `new_parent`:

| Case | Behaviour (matches Linux) |
|---|---|
| Target doesn't exist | As before; needs free space in the folder (`ENOSPC` if full) |
| Target is the same inode (two hard links to one file) | Do nothing, succeed |
| File onto a folder | `EISDIR` |
| Folder onto a file | `ENOTDIR` |
| Folder onto a non-empty folder | `ENOTEMPTY` |
| Otherwise | Remove the target's entry, then add the new one |

When removing the target:
- **File:** `hard_links -= 1`; if it reaches 0, free its blocks and inode (same steps as `unlink`). Other hard links keep the file alive.
- **Empty folder:** free its block and inode (same steps as `rmdir`), and decrease the parent's link count (the removed folder's `..` no longer points at it).

The space check is skipped when a target exists. Removing the entry frees exactly the space the same name needs, so replacing a name now works in a full folder (before, it returned `ENOSPC`).

All checks happen **before** the first write, so a refused rename changes nothing (same rule as the full-directory fix).

**New helper `free_inode`:** truncate to 0, clear the inode bitmap bit, `free_inodes += 1`, write the superblock. These are the same steps `unlink`/`rmdir` do inline; they were left untouched to keep this change small.

**Also fixed (one line):** a renamed symlink's directory entry was recorded as `FT_REGULAR`; it now keeps `FT_SYMLINK`.

#### Verification: `scripts/e2e_test.sh` (new)

Each case mounts a fresh image (on ext4), runs real commands, **remounts** to defeat the kernel's caches, checks the result, and runs fsck. Run against both builds; the old build is `HEAD` (`ead0734`) built in a separate `git worktree`:

| Case | Old code | New code |
|---|---|---|
| Move a folder with contents into another folder | contents missing; fsck: 2 orphaned inodes + link mismatch | ok, fsck CONSISTENT |
| `mv a b` onto existing `b` | old content after remount; two `b` entries | ok |
| Editor-style save (temp file + rename) | **reverted to v1 after remount** | ok |
| Rename onto a hard-linked file | other link's count not decreased | ok |
| Folder onto non-empty folder | **accepted; target's contents lost** | refused (`ENOTEMPTY`) |
| File onto folder / folder onto file | refused | refused |
| Renamed symlink | works | works |
| Rename onto an existing name in a full folder | `ENOSPC` | ok |
| **Total** | **18 passed, 12 failed** | **30 passed, 0 failed** |

Two cases passed on the old code as well:
- **Type mismatches:** the kernel's VFS refuses those itself before calling the daemon, so the new `EISDIR`/`ENOTDIR` checks are a backup safety check.
- **Symlink:** reading a symlink uses the inode's `mode`, not the entry's type byte. The wrong type byte only affected `readdir`'s `d_type`.

`cargo test -p libfs`: 9 passed.

#### Not fixed (noted)

- fsck reported CONSISTENT on directories with duplicate names. *(Fixed in the next entry.)*
- `renameat2` flags were still ignored. *(Fixed in the next entry.)*
- Renaming to a **new** name in a full folder still returned `ENOSPC`. *(Fixed in the next entry for the same folder.)*
- Like most handlers except `create`, `rename` is not journaled; a crash in the middle can leave it half done.

---

### Fixed: every inode write overwrote the next inode's `mode` and `uid`

**Date:** 2026-11-16
**Files:** `libfs/src/layout.rs`, `libfs/src/inode.rs`, `libfs/src/dir.rs`, `mkfs/src/main.rs`, `rustfs/src/main.rs`, `libfs/examples/bench_dir_lookup.rs`
**Status:** Verified (unit tests + end-to-end reproduction, before and after)

#### The problem

Inode slots on disk are `INODE_SIZE` = **256 bytes**, but `struct Inode` was **264 bytes**. `#[repr(C)]` puts every `u64` on an 8-byte boundary, so the compiler adds hidden padding the `_padding` field did not account for:

```
mode u32, uid u32, gid u32      0..12
  [4 hidden bytes]             12..16   size (u64) must start at a multiple of 8
size, ctime, atime, mtime      16..48
hard_links u32                 48..52
  [4 hidden bytes]             52..56   direct[] (u64) must start at a multiple of 8
direct[12], indirect, d_ind    56..168
_padding [u8; 96]             168..264  ← should have been 88
```

`write_inode` and the inode writes in `create`/`mkdir`/`symlink` copy the whole struct with `ptr::write`, so every write was 264 bytes into a 256-byte slot:

- **Slots 0–14:** the last 8 bytes landed on the **first 8 bytes of the next inode**, which hold its `mode` (file type and permissions) and `uid`.
- **Slot 15** (offset 3840): 3840 + 264 = 4104, so the write went **8 bytes past the 4 KiB block buffer**. `read_inode` read 8 bytes past it too. Both are undefined behaviour.

#### Why it went unnoticed

- **Read, modify, write updates restored the neighbour.** `read_inode` also reads 264 bytes, so the end of `_padding` holds the next inode's current `mode`/`uid`, and `write_inode` writes the same values back. The `dd` benchmark only does this kind of update, so it couldn't show the bug, and old and new code both had it.
- **New inodes were always followed by free ones.** A newly built inode has zero padding, which wipes the next slot. On a fresh image inodes are handed out in order (3, 4, 5…), so the wiped slot was always unused.
- It is a correctness bug, not a speed bug: 8 extra bytes per write cost nothing measurable.

#### How it was reproduced (before the fix)

Inode N is at byte `12288 + 256 × N` in the image (inode table starts at block 3), so inode 4 is at 13312.

```bash
touch /tmp/mnt/a; mkdir /tmp/mnt/d    # a = inode 3, d = inode 4
# unmount → xxd -s 13312 -l 8 myfs.img →  fd41 0000 e803 0000   (dir 0o40775, uid 1000)
rm /tmp/mnt/a; touch /tmp/mnt/c       # c reuses inode 3; its zero padding hits inode 4
# unmount → xxd -s 13312 -l 8 myfs.img →  0000 0000 0000 0000   (mode and uid wiped)
```

After remounting:

```
---------- 2 root  hemal 4096 Nov 16 18:31 d      ← was drwxrwxr-x hemal
bash: cd: /tmp/mnt/d: Not a directory
```

- `gid` (bytes 8–12) survived, which shows exactly 8 bytes were overwritten.
- The directory's data block and entries were intact but could no longer be reached.
- **fsck reported the wrong cause:** `root inode has hard_links = 3 but expected 2` and `inode 4 has hard_links = 2 but expected 1`. The stored link counts were correct. fsck no longer saw inode 4 as a directory, so it didn't count its `.` and `..`. A repair that "fixed" those counts would have made the damage worse.

#### What could have happened

Any user could trigger it with ordinary `rm` and create commands. Files and directories whose inode came right after a reused one lost their type and owner after a remount. A directory became an unreadable root-owned file and its contents were cut off. The 8-byte write past the buffer at slot 15 could, after a compiler or code change, overwrite a live stack variable and crash in an unrelated place.

#### The fix

1. **`libfs/src/layout.rs`:** `_padding: [u8; 96]` → `[u8; 88]`, with a comment explaining where the hidden 8 bytes come from.
2. **Compile-time size check** below the struct:
   ```rust
   const _: () = assert!(std::mem::size_of::<Inode>() == INODE_SIZE as usize);
   ```
   If a field is ever added or reordered and the size drifts, the build fails instead of corrupting disks.
3. **Every inode constructor** updated from `[0u8; 96]` to `[0u8; 88]`: `rustfs/src/main.rs` (3 places: create, mkdir, symlink), `mkfs/src/main.rs` (root inode), `libfs/src/dir.rs` (test), `libfs/examples/bench_dir_lookup.rs`.

**On-disk compatibility:** every field keeps the same offset; only unused padding at the end of the struct got shorter. Existing images still open. **Images already damaged are not repaired**: an inode whose `mode` was wiped stays wiped.

#### Tests added (`libfs/src/inode.rs`)

| Test | Checks | Old code | New code |
|---|---|---|---|
| `inode_is_exactly_one_slot` | `size_of::<Inode>() == 256` | FAILED (264) | ok |
| `new_inode_does_not_overwrite_next_inode` | Write a directory into inode 4, then a new file into inode 3; inode 4's `mode`/`uid` unchanged | FAILED (`mode` = 0) | ok |
| `every_slot_in_a_block_round_trips` | Write all 16 slots of a block **in reverse order** (including slot 15) and read each back | FAILED | ok |

The "old code" column was produced by running the new tests against `HEAD` in a separate `git worktree`. The slot test writes in reverse because in increasing order each write re-stamps the bytes the previous one spilled onto, which hides the bug.

#### Verification (after the fix)

- `cargo test -p libfs`: **9 passed, 0 failed**.
- Same end-to-end sequence on a fresh image (release build, image on ext4):
  ```
  after mkdir d:          fd41 0000 e803 0000
  after rm a + touch c:   fd41 0000 e803 0000     ← unchanged
  drwxrwxr-x 2 hemal hemal 4096 … d
  cd d: OK
  fsck: FileSystem is CONSISTENT. No errors found.
  ```

#### Follow-up (not done)

fsck should check that every inode marked in use has a valid file type (`mode & 0o170000` is a regular file, a directory or a symlink). Then this kind of damage is reported as what it is instead of as link-count mismatches.

---

### Fixed: a full directory crashed the filesystem instead of returning ENOSPC

**Date:** 2026-11-16
**Files:** `libfs/src/dir.rs`, `rustfs/src/main.rs`, `libfs/examples/bench_dir_lookup.rs`

#### The problem

A directory's entries all live in one 4 KiB block (`inode.direct[0]`). Short names like `f1` … `f999` take 12 bytes each, so a block holds about **341 entries** (including `.` and `..`).

When the block was nearly full, `prepare_add_entry` and `add_entry` shrank the last entry and wrote the new one after it **without checking that it fit**. Found by creating files in a loop in the root directory:

```
341 entries × 12 bytes = 4,092 bytes → 4 bytes left, new entry needs 12
→ the name copy indexed block[4100..4104]
→ panicked at libfs/src/dir.rs:351: "range start index 4100 out of range for slice of length 4096"
```

#### What happened as a result

- The FUSE daemon crashed. The pending `create` failed with **Input/output error**.
- The mount uses `MountOption::AutoUnmount`, so the mount disappeared and `ls` showed an empty `/tmp/mnt` (0 files). Your files weren't lost: fsck on the image afterwards was CONSISTENT, with `free_inodes 6058` (all 339 files present).
- Any user could crash the filesystem by putting about 341 entries in one directory. `mkdir`, `symlink`, `link` and `rename` into a full directory crashed the same way. They also hit the `panic!("directory block full…")` at the end of the function.

#### The fix

**1. `libfs/src/dir.rs`: refuse instead of overflowing**

- `prepare_add_entry` now returns `Option<(u64, [u8; 4096])>`. Before appending after the last entry, it checks `remaining < actual_size` and returns `None` without changing anything. The final `panic!` is now `None`.
- `add_entry` was a line-for-line copy of `prepare_add_entry` plus a write. It now calls `prepare_add_entry` and writes the result, returning `Option<()>`, so the space check lives in one place.
- New `has_space_for(dir, name) -> bool`: a dry run of `prepare_add_entry` that writes nothing.

**2. `rustfs/src/main.rs`: return ENOSPC before changing anything**

A new helper, `RustFS::dir_has_space`, wraps `has_space_for`. Most handlers allocate an inode or block, or bump a link count, *before* adding the directory entry, so a late failure would leave an orphan. They now check first:

| Handler | Change |
|---|---|
| `create` | Matches on `prepare_add_entry`; `None` → `ENOSPC`. Already safe here, because nothing is written until the journal transaction that follows. |
| `mkdir` | `dir_has_space` check at the top, before the inode or the data block is allocated |
| `symlink` | Check at the top, before the inode is allocated |
| `link` | Check at the top, before `hard_links += 1` |
| `rename` | Check before the inode's ctime is written and before the old entry is removed |

The later `add_entry` calls use `.expect(...)`, since space was already checked and the FUSE session handles one request at a time.

**3. `libfs/examples/bench_dir_lookup.rs`:** updated for the new return type (`.expect`). The benchmark's largest count (300) fits in one block.

#### Tests added (`libfs/src/dir.rs`)

| Test | Checks |
|---|---|
| `full_directory_returns_none_instead_of_panicking` | Exactly 341 twelve-byte entries fit. The next add returns `None` from `has_space_for`, `prepare_add_entry` and `add_entry`, the block is byte-for-byte unchanged, and all 341 entries are still listed and found by `look_up`. |
| `removed_slot_is_reused_when_full` | In a full directory, removing one entry frees its slot, a new name of the same size fits there, and the directory is full again afterwards. |

#### Verification status

**Verified on 2026-11-16** (Linux, cargo 1.96.0, debug build, fresh `mkfs` image):

- `cargo test -p libfs`: **6 passed**, 0 failed. This includes `full_directory_returns_none_instead_of_panicking` and `removed_slot_is_reused_when_full`.
- End to end, creating files in the root directory through the FUSE mount:

```
$ for i in $(seq 1 7000); do : > /tmp/mnt/f$i || { echo "stopped at $i"; break; }; done
bash: /tmp/mnt/f340: No space left on device
stopped at 340
$ ls /tmp/mnt | wc -l
339
$ mkdir /tmp/mnt/x
mkdir: cannot create directory '/tmp/mnt/x': No space left on device
```

  The daemon kept running with no panic. Before this fix, the same loop crashed the daemon at `f340` with `Input/output error`.
- After unmounting, `fsck` reported `free_inodes 6058`, `free_blocks 25188`, **CONSISTENT**. The refused `mkdir` left no orphaned inode or block.

#### Not addressed here

- Directories are still limited to one block (about 341 short names, fewer with longer names). Growing a directory into `direct[1..]` would lift the limit.
- `rename` checks for space even when renaming within the same directory, where removing the old name could free enough room. That case returns `ENOSPC` in a full directory.
- Names over 255 bytes overflow the `name_len: u8` field. FUSE normally rejects these first, but the code doesn't check.

---

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

**Verified on 2026-11-16** (Linux, cargo 1.96.0, clean build):

- `cargo run --bin mkfs` printed `Layout: inode table 3-402 (6400 inodes), journal 403-410, data 411-25599 (25189 blocks)`.
- `fsck` on the new image: `total_inodes 6400`, `free_inodes 6397`, `free_blocks 25188`, **CONSISTENT**.
- Filling the filesystem through FUSE (40 subdirectories of 200 files each, so no single directory fills up) created **6,397** inodes before the first refusal. Before this change, the limit was 2,045. See the allocator entry below for the full run.

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

**Verified on 2026-11-16** (Linux, cargo 1.96.0, clean build):

- `cargo test -p libfs`: `alloc_stops_at_limit`, `alloc_limit_not_multiple_of_8` and `free_bit_is_reused` all pass.
- End to end, exhausting every inode through the FUSE mount on a fresh image:

```
$ for d in $(seq 1 40); do
    mkdir /tmp/mnt/d$d || { echo "mkdir stopped at d$d"; break; }
    for f in $(seq 1 200); do
      : > /tmp/mnt/d$d/f$f || { echo "stopped at d$d/f$f"; break 2; }
    done
  done
bash: /tmp/mnt/d32/f166: No space left on device
stopped at d32/f166
```

  This is the expected stopping point: 6,397 free inodes = 31 × (1 dir + 200 files) + 1 dir + 165 files. The daemon did not panic and shut down cleanly on unmount.
- `fsck` afterwards: `free_inodes 0`, `free_blocks 25156` (25,188 − 32 directory blocks), **CONSISTENT**. No inode was handed out past the end of the inode table.

#### Known remaining issues (not fixed by this change)

- `mkdir` allocates the inode before the data block. If the data block allocation then fails, the inode stays marked used but is unreachable. fsck reports it as an orphaned inode.
- The `Inode` struct is 264 bytes under `#[repr(C)]`, but inode slots are 256 bytes, so each inode write overlaps the next inode's `mode` and `uid`. Tracked separately.
