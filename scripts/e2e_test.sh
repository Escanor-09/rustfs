#!/usr/bin/env bash
# End-to-end tests: mount a fresh image, run real commands through FUSE,
# remount (to defeat the kernel's caches), check results and run fsck.
#
# usage: scripts/e2e_test.sh [BIN_DIR] [WORK_DIR]
#   BIN_DIR   directory holding mkfs, rustfs, fsck (default: target/release)
#   WORK_DIR  where the scratch image lives; keep it on ext4, not NTFS
#             (default: ~/rustfs-bench)
#
# Each case gets a fresh image. Never run while another rustfs is mounted
# on /tmp/mnt.

set -u
BIN=$(realpath "${1:-$(dirname "$0")/../target/release}")
WORK=${2:-$HOME/rustfs-bench}
MNT=/tmp/mnt
IMG=$WORK/e2e.img
LOG=$WORK/e2e-daemon.log
mkdir -p "$MNT" "$WORK"

pass=0; fail=0

mnt() {
    ("$BIN/rustfs" "$IMG" "$MNT" >>"$LOG" 2>&1 &)
    for _ in $(seq 50); do mountpoint -q "$MNT" && return 0; sleep 0.1; done
    echo "  mount failed"; return 1
}
um() {
    fusermount3 -u "$MNT" 2>/dev/null
    while pgrep -x rustfs >/dev/null; do sleep 0.1; done
}
fresh() { um; rm -f "$IMG" "$LOG"; (cd "$WORK" && "$BIN/mkfs" >/dev/null && mv myfs.img "$IMG"); mnt; }
remount() { um; mnt; }
check() { # check "description" <command...>
    local desc=$1; shift
    if "$@" >/dev/null 2>&1; then echo "  ok    $desc"; pass=$((pass+1))
    else echo "  FAIL  $desc"; fail=$((fail+1)); fi
}
fsck_ok() {
    um
    if "$BIN/fsck" "$IMG" | grep -q CONSISTENT; then echo "  ok    fsck CONSISTENT"; pass=$((pass+1))
    else echo "  FAIL  fsck:"; "$BIN/fsck" "$IMG" | grep -E "ORPHAN|MISMATCH|DUPLICATE|Found" | sed 's/^/          /'; fail=$((fail+1)); fi
    if grep -q panicked "$LOG" 2>/dev/null; then echo "  FAIL  daemon panicked"; fail=$((fail+1)); fi
}
count_name() { ls -1a "$MNT/$1" | grep -cx "$2"; }

echo "== move a directory (with contents) into another directory"
fresh
mkdir $MNT/d1 $MNT/d2; echo hello > $MNT/d1/f; mkdir $MNT/d1/sub
mv $MNT/d1 $MNT/d2/
remount
check "moved dir still contains f"           test "$(cat $MNT/d2/d1/f)" = hello
check "moved dir still contains sub/"        test -d $MNT/d2/d1/sub
check "moved dir .. is the new parent"       test "$(stat -c %i $MNT/d2/d1/..)" = "$(stat -c %i $MNT/d2)"
check "root link count 3 (., .., d2/..)"     test "$(stat -c %h $MNT)" = 3
check "d2 link count 3 (name, ., d1/..)"     test "$(stat -c %h $MNT/d2)" = 3
fsck_ok

echo "== rename a file over an existing file"
fresh
echo AAA > $MNT/a; echo BBB > $MNT/b
mv $MNT/a $MNT/b
remount
check "b has the new content after remount"  test "$(cat $MNT/b)" = AAA
check "exactly one entry named b"            test "$(count_name . b)" = 1
check "a is gone"                            test ! -e $MNT/a
fsck_ok   # also proves the replaced inode was freed (no orphan)

echo "== editor-style save (write temp file, rename over original)"
fresh
echo v1 > $MNT/doc.txt
echo v2 > $MNT/.doc.txt.tmp && mv $MNT/.doc.txt.tmp $MNT/doc.txt
remount
check "doc.txt is v2 after remount"          test "$(cat $MNT/doc.txt)" = v2
fsck_ok

echo "== rename over a hard-linked file keeps the other link"
fresh
echo OLD > $MNT/b; ln $MNT/b $MNT/b2; echo NEW > $MNT/a
mv $MNT/a $MNT/b
remount
check "b is NEW"                             test "$(cat $MNT/b)" = NEW
check "b2 still has OLD"                     test "$(cat $MNT/b2)" = OLD
check "b2 link count back to 1"              test "$(stat -c %h $MNT/b2)" = 1
fsck_ok

echo "== rename a directory over an empty / non-empty directory"
fresh
mkdir -p $MNT/src $MNT/empty $MNT/full; touch $MNT/src/x $MNT/full/y
mv -T $MNT/src $MNT/empty
check "dir over empty dir succeeds"          test -e $MNT/empty/x
check "dir over non-empty dir is refused"    bash -c "! mv -T $MNT/empty $MNT/full"
remount
check "non-empty target untouched"           test -e $MNT/full/y
check "root link count 4 (., .., empty, full)" test "$(stat -c %h $MNT)" = 4
fsck_ok

echo "== type mismatches are refused"
fresh
mkdir $MNT/d; touch $MNT/f
check "file over dir refused"                bash -c "! mv -T $MNT/f $MNT/d"
check "dir over file refused"                bash -c "! mv -T $MNT/d $MNT/f"
check "both still exist"                     test -d $MNT/d -a -f $MNT/f
fsck_ok

echo "== renamed symlink keeps working"
fresh
echo hi > $MNT/t; ln -s t $MNT/s; mv $MNT/s $MNT/s2
remount
check "symlink still a symlink"              test -L $MNT/s2
check "symlink still resolves"               test "$(cat $MNT/s2)" = hi
fsck_ok

echo "== rename inside a full directory (replacing an existing name)"
fresh
for i in $(seq 1 339); do : > $MNT/f$i; done   # 339 + . + .. = full block
mv $MNT/f2 $MNT/f1
remount
check "rename over existing name works when full" bash -c "test ! -e $MNT/f2 -a -e $MNT/f1"
fsck_ok

echo "== rename to a NEW name inside a full directory"
fresh
for i in $(seq 1 339); do : > $MNT/f$i; done
mv $MNT/f1 $MNT/g1 2>/dev/null
check "same-length new name fits (old slot reused)" test -e $MNT/g1 -a ! -e $MNT/f1
mv $MNT/f2 $MNT/a_much_longer_name_that_cannot_fit 2>/dev/null
check "longer name refused"                  test ! -e $MNT/a_much_longer_name_that_cannot_fit
check "refused rename keeps the old name"    test -e $MNT/f2
remount
check "339 entries after remount"            test "$(ls $MNT | wc -l)" = 339
fsck_ok

echo "== renameat2 flags"
fresh
echo A > $MNT/a; echo B > $MNT/b
r2() { python3 -c "
import ctypes,os,sys
libc=ctypes.CDLL(None,use_errno=True); AT=-100
r=libc.renameat2(AT,sys.argv[1].encode(),AT,sys.argv[2].encode(),int(sys.argv[3]))
print(os.strerror(ctypes.get_errno()) if r else 'ok')" "$@"; }
check "RENAME_EXCHANGE refused (not supported)" test "$(r2 $MNT/a $MNT/b 2)" = "Invalid argument"
check "RENAME_NOREPLACE onto existing refused"  test "$(r2 $MNT/a $MNT/b 1)" = "File exists"
remount
check "both files untouched"                 test "$(cat $MNT/a)$(cat $MNT/b)" = AB
fsck_ok

echo "== disk full: mkdir and write"
fresh
dd if=/dev/zero of=$MNT/fill bs=1M count=200 2>$WORK/dd.err
check "full-disk write says No space left"   grep -q "No space left on device" $WORK/dd.err
check "mkdir on a full disk refused"         bash -c "! mkdir $MNT/newdir"
fsck_ok   # old code: ORPHANED INODE (mkdir leaked it)

echo "== write integrity (large, unaligned, cross-block, appends)"
fresh
head -c 300000 /dev/urandom > $WORK/rand
cp $WORK/rand $MNT/big
python3 -c "
import os
f=os.open('$MNT/cross',os.O_CREAT|os.O_WRONLY,0o644); os.pwrite(f,b'Z'*200,4000); os.close(f)
f=os.open('$MNT/app',os.O_CREAT|os.O_WRONLY|os.O_APPEND,0o644)
for i in range(50): os.write(f,bytes([65+i%26])*997)
os.close(f)"
remount
check "300 KB random file identical"         cmp -s $WORK/rand $MNT/big
check "cross-block pwrite correct"           python3 -c "d=open('$MNT/cross','rb').read(); assert d==bytes(4000)+b'Z'*200"
check "unaligned appends correct"            python3 -c "d=open('$MNT/app','rb').read(); assert d==b''.join(bytes([65+i%26])*997 for i in range(50))"
fsck_ok

echo "== clear errors instead of panics"
um
out=$("$BIN/rustfs" "$IMG" /tmp/does-not-exist-$$ 2>&1); rc=$?
check "missing mount point: exit 1"          test $rc = 1
check "missing mount point: clear message"   bash -c "grep -q 'not a directory' <<< \"$out\" && ! grep -q panicked <<< \"$out\""
out=$("$BIN/rustfs" /tmp/no-such-image-$$.img "$MNT" 2>&1); rc=$?
check "missing image: exit 1 with message"   bash -c "test $rc = 1 && grep -q 'not found' <<< \"$out\""

echo "== fsck detects damaged file type and duplicate names"
# inode N lives at byte inode_table_block*4096 + N*256 (inode table at block 3)
fresh; mkdir $MNT/d; um                        # d = inode 3
python3 -c "
f=open('$IMG','r+b'); f.seek(3*4096+3*256); f.write(b'\0\0\0\0')"   # wipe d's mode
check "fsck reports INVALID FILE TYPE"       bash -c "'$BIN/fsck' '$IMG' | grep -q 'INVALID FILE TYPE: inode 3'"
fresh; : > $MNT/ab; : > $MNT/ac; um
python3 -c "
f=open('$IMG','r+b'); d=bytearray(f.read())
i=d.rindex(b'\x02\x01ac'); d[i+2:i+4]=b'ab'   # last copy = the real folder block (earlier copies are in the journal)
f.seek(0); f.write(d)"
check "fsck reports DUPLICATE NAME"          bash -c "'$BIN/fsck' '$IMG' | grep -q \"DUPLICATE NAME: 'ab'\""

um
echo
echo "passed: $pass   failed: $fail"
[ "$fail" -eq 0 ]
