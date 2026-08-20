#!/bin/bash
# verify-on-linux.sh — put our filesystems in front of a real kernel.
#
# The unit tests prove we can read back our own boot sector, which proves very
# little. This builds images with the `mkimage` example, ships them to a Linux
# host, and runs each one through fsck.fat, a read-write mount, a write, an
# unmount and a second fsck.fat.
#
# The second fsck.fat is the one that matters: a filesystem the kernel mounts,
# writes to, and then leaves consistent is a working filesystem. "Mounts" and
# "is writable" are different claims, and only the write proves the second.
#
#   ./tests/verify-on-linux.sh [user@host]
#
# Defaults to root@dev.g8.lo.

set -uo pipefail

HOST="${1:-root@dev.g8.lo}"
REMOTE_DIR=/root/mkfs-dos-verify
FAILURES=0

GREEN='' RED='' CYAN='' BOLD='' RESET=''
if [ -t 1 ]; then
    GREEN='\033[0;32m'; RED='\033[0;31m'; CYAN='\033[0;36m'
    BOLD='\033[1m'; RESET='\033[0m'
fi
ok()  { echo -e "  ${GREEN}OK${RESET}: $1"; }
bad() { echo -e "  ${RED}FAIL${RESET}: $1"; FAILURES=$((FAILURES+1)); }
hdr() { echo; echo -e "${BOLD}${CYAN}-- $1 --${RESET}"; }

# name:size:write-kib:extra mkimage args
# The size is MiB unless it carries a 'k'. The write size is what the kernel is
# asked to write once mounted, and has to fit — a 1.44 MB floppy cannot take the
# 2 MiB the larger volumes get.
CASES=(
    "fat12-8m:8:2048:"
    "fat12-32m-forced:32:2048:12"
    "fat16-64m:64:2048:"
    "fat16-256m:256:2048:label=SIXTEEN"
    "fat16-64m-onefat:64:2048:fats=1"
    "fat16-64m-noalign:64:2048:noalign"
    "fat32-512m:512:2048:"
    "fat32-1g:1024:2048:label=ESP"
    "fat32-1g-4k:1024:2048:sector=4096"
    "fat32-64m-forced:64:2048:32"
    # A real 1.44 MB floppy, where the historical parameters apply.
    "fat12-1440k:1440k:400:"
)

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

hdr "building images"
# The example builds the images and the fsck-fat binary checks them afterwards,
# so both have to be current — a stale binary here checks last run's code.
cargo build --quiet --release --example mkimage --bin fsck-fat || {
    echo "cargo build failed"; exit 1;
}

for case in "${CASES[@]}"; do
    IFS=: read -r name size writekib extra <<< "$case"
    ./target/release/examples/mkimage "$WORK/$name.img" "$size" $extra > /dev/null || {
        bad "$name: mkimage failed"; continue;
    }
done

hdr "shipping to $HOST"
ssh "$HOST" "rm -rf $REMOTE_DIR && mkdir -p $REMOTE_DIR" || exit 1
scp -q "$WORK"/*.img "$HOST:$REMOTE_DIR/" || exit 1

for case in "${CASES[@]}"; do
    IFS=: read -r name size writekib extra <<< "$case"
    hdr "$name ($size) $extra"

    # Everything the kernel and dosfstools have to say about the image, in one
    # round trip: check it, mount it read-write, write to it, unmount, check it
    # again. The mount is what a driver does; the second check is what it left.
    out=$(ssh "$HOST" "bash -s" <<REMOTE
set -uo pipefail
cd $REMOTE_DIR
img=$name.img
echo "### fsck-before"
fsck.fat -n -v \$img 2>&1 | tail -20
echo "### mount"
mkdir -p /mnt/$name
mount -o loop -t vfat \$img /mnt/$name 2>&1 && echo MOUNTED
echo "### write"
if mountpoint -q /mnt/$name; then
    mkdir -p /mnt/$name/adir/deeper 2>&1 && echo MKDIR-OK
    echo hello > /mnt/$name/hello.txt 2>&1 && echo WRITE-OK
    dd if=/dev/urandom of=/mnt/$name/big.bin bs=1K count=$writekib 2>/dev/null && echo BIGWRITE-OK
    cp /mnt/$name/big.bin /mnt/$name/adir/deeper/copy.bin && echo COPY-OK
    echo "a long file name that needs a long directory entry" > "/mnt/$name/A Long File Name.txt" && echo LFN-OK
    cmp /mnt/$name/big.bin /mnt/$name/adir/deeper/copy.bin && echo COMPARE-OK
    sync
    umount /mnt/$name && echo UNMOUNTED
fi
echo "### fsck-after"
fsck.fat -n -v \$img 2>&1 | tail -25
echo "### mount-again"
mount -o loop -t vfat \$img /mnt/$name 2>&1 && ls /mnt/$name && cat /mnt/$name/hello.txt && umount /mnt/$name && echo REREAD-OK
rmdir /mnt/$name
REMOTE
)

    echo "$out" | grep -q "MOUNTED"      && ok "kernel mounted it read-write"       || bad "mount failed"
    echo "$out" | grep -q "WRITE-OK"     && ok "wrote a file"                       || bad "write failed"
    echo "$out" | grep -q "MKDIR-OK"     && ok "created nested directories"         || bad "mkdir failed"
    echo "$out" | grep -q "BIGWRITE-OK"  && ok "wrote ${writekib} KiB"               || bad "large write failed"
    echo "$out" | grep -q "COMPARE-OK"   && ok "read it back byte for byte"         || bad "compare failed"
    echo "$out" | grep -q "LFN-OK"       && ok "wrote a long file name"             || bad "long name failed"
    echo "$out" | grep -q "UNMOUNTED"    && ok "unmounted cleanly"                  || bad "unmount failed"
    echo "$out" | grep -q "REREAD-OK"    && ok "remounted and read it back"         || bad "remount failed"

    before=$(echo "$out" | sed -n '/### fsck-before/,/### mount/p')
    after=$(echo "$out" | sed -n '/### fsck-after/,/### mount-again/p')
    echo "$before" | grep -qE "Dirty bit|Free cluster summary wrong|Bad|error|Cluster" \
        && { bad "fsck.fat complained before the mount"; echo "$before" | sed 's/^/      /'; } \
        || ok "fsck.fat clean before the mount"
    echo "$after" | grep -qE "Dirty bit|Free cluster summary wrong|Bad|error|Cluster" \
        && { bad "fsck.fat complained after the write"; echo "$after" | sed 's/^/      /'; } \
        || ok "fsck.fat clean after the write"

    # And our own checker must agree with dosfstools about the same image.
    scp -q "$HOST:$REMOTE_DIR/$name.img" "$WORK/$name.after.img"
    if ./target/release/fsck-fat "$WORK/$name.after.img" > "$WORK/$name.fsck" 2>&1; then
        ok "our fsck.fat calls it clean after the kernel wrote to it"
    else
        bad "our fsck.fat disagrees"; sed 's/^/      /' "$WORK/$name.fsck"
    fi
done

hdr "result"
if [ "$FAILURES" -eq 0 ]; then
    echo -e "${GREEN}all configurations pass${RESET}"
else
    echo -e "${RED}$FAILURES check(s) failed${RESET}"
fi
exit $((FAILURES > 0))
