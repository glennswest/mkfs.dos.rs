#!/bin/bash
# verify-on-linux.sh — put our filesystems in front of other FAT implementations.
#
# The unit tests prove we can read back our own boot sector, which proves very
# little. This builds images with the `mkimage` example and runs each one
# through implementations that are not ours:
#
#   1. fsck.fat -n          dosfstools' checker, on the fresh image
#   2. mtools write         mmd, mcopy (2 MiB file, a long file name), read back
#                           and compare, then fsck.fat -n and our fsck-fat again
#   3. kernel write         loop mount read-write, write, mkdir -p, long name,
#                           compare, unmount, fsck.fat -n, our fsck-fat, remount
#
# Stages 1 and 2 need no privilege and run where the script runs, so the
# day-to-day way to run it is on the build box, unprivileged (#1):
#
#   sc-build tests/verify-on-linux.sh
#
# Stage 3 needs a loop device, which means root: it runs when the script runs
# as root (a privileged test container, or a Linux machine of your own) and is
# skipped otherwise — reported as SKIP, never as a pass.
#
#   tests/verify-on-linux.sh [--require-mount]
#
# --require-mount (or VERIFY_REQUIRE_MOUNT=1) turns a skipped kernel stage into
# a failure, for a run whose point is the kernel. A missing mtools is the same:
# SKIP, unless --require-mtools (VERIFY_REQUIRE_MTOOLS=1).
#
# The fsck.fat after a write is the one that matters: a filesystem another
# implementation writes to and then leaves consistent is a working filesystem.
# "Mounts" and "is writable" are different claims, and only the write proves
# the second. Our own fsck-fat must then agree about the same image, which
# makes its verdict testable against a filesystem it did not create.
#
# Nothing is written outside a temporary directory (under $TMPDIR).

set -uo pipefail

REQUIRE_MOUNT="${VERIFY_REQUIRE_MOUNT:-0}"
REQUIRE_MTOOLS="${VERIFY_REQUIRE_MTOOLS:-0}"
for arg in "$@"; do
    case "$arg" in
        --require-mount)  REQUIRE_MOUNT=1 ;;
        --require-mtools) REQUIRE_MTOOLS=1 ;;
        *) echo "usage: $0 [--require-mount] [--require-mtools]" >&2; exit 2 ;;
    esac
done

FAILURES=0
SKIPS=0

GREEN='' RED='' YELLOW='' CYAN='' BOLD='' RESET=''
if [ -t 1 ]; then
    GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[0;33m'; CYAN='\033[0;36m'
    BOLD='\033[1m'; RESET='\033[0m'
fi
ok()   { echo -e "  ${GREEN}OK${RESET}: $1"; }
bad()  { echo -e "  ${RED}FAIL${RESET}: $1"; FAILURES=$((FAILURES+1)); }
skip() { echo -e "  ${YELLOW}SKIP${RESET}: $1"; SKIPS=$((SKIPS+1)); }
hdr()  { echo; echo -e "${BOLD}${CYAN}-- $1 --${RESET}"; }
# check <description> <command...> — OK if the command succeeds, FAIL if not.
check() { local what="$1"; shift; if "$@"; then ok "$what"; else bad "$what"; fi; }

# name:size:write-kib:extra mkimage args
# The size is MiB unless it carries a 'k'. The write size is what is written
# into the volume (twice: the file and its copy), and has to fit — a 1.44 MB
# floppy cannot take the 2 MiB the larger volumes get.
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

# fsck.fat lives in /usr/sbin, which an unprivileged user's PATH may lack.
PATH="$PATH:/usr/sbin:/sbin"

hdr "environment"
FSCK_FAT=$(command -v fsck.fat) || { echo "fsck.fat not found — install dosfstools"; exit 1; }
echo "  fsck.fat: $FSCK_FAT ($("$FSCK_FAT" --help 2>&1 | head -1))"
HAVE_MTOOLS=0
if command -v mcopy > /dev/null && command -v mmd > /dev/null; then
    HAVE_MTOOLS=1
    echo "  mtools:   $(mcopy --version 2>&1 | head -1)"
else
    echo "  mtools:   not installed"
fi
CAN_MOUNT=0
if [ "$(id -u)" -eq 0 ]; then
    CAN_MOUNT=1
    echo "  kernel:   $(uname -sr), running as root — loop mounts enabled"
else
    echo "  kernel:   $(uname -sr), not root — kernel stage will be skipped"
fi

WORK="$(mktemp -d)"
cleanup() {
    for m in "$WORK"/mnt/*; do mountpoint -q "$m" 2>/dev/null && umount "$m"; done
    rm -rf "$WORK"
}
trap cleanup EXIT

hdr "building"
# The example builds the images and the fsck-fat binary checks them afterwards,
# so both have to be current — a stale binary here checks last run's code.
cargo build --quiet --release --example mkimage --bin fsck-fat || {
    echo "cargo build failed"; exit 1;
}
TARGET="${CARGO_TARGET_DIR:-target}/release"
MKIMAGE="$TARGET/examples/mkimage"
OUR_FSCK="$TARGET/fsck-fat"

# fsck.fat -n is clean when it exits 0 and says nothing alarming.
dosfsck_clean() {
    local img="$1" log="$2"
    "$FSCK_FAT" -n -v "$img" > "$log" 2>&1
    local rc=$?
    [ "$rc" -eq 0 ] && ! tail -25 "$log" | grep -qE "Dirty bit|Free cluster summary wrong|Bad|error|Cluster"
}
dosfsck_check() {
    if dosfsck_clean "$1" "$2"; then ok "$3"; else bad "$3"; tail -25 "$2" | sed 's/^/      /'; fi
}
ourfsck_check() {
    if "$OUR_FSCK" "$1" > "$2" 2>&1; then ok "$3"; else bad "$3"; sed 's/^/      /' "$2"; fi
}

for case in "${CASES[@]}"; do
    IFS=: read -r name size writekib extra <<< "$case"
    hdr "$name ($size) $extra"
    img="$WORK/$name.img"
    data="$WORK/$name.data"
    mkdir -p "$data"

    "$MKIMAGE" "$img" "$size" $extra > "$data/mkimage.log" 2>&1 || {
        bad "mkimage failed"; sed 's/^/      /' "$data/mkimage.log"; continue;
    }
    echo hello > "$data/hello.txt"
    echo "a long file name that needs a long directory entry" > "$data/lfn.txt"
    dd if=/dev/urandom of="$data/big.bin" bs=1K count="$writekib" 2>/dev/null

    # -- 1. dosfstools on the image as we wrote it
    dosfsck_check "$img" "$data/fsck-fresh.log" "fsck.fat clean on the fresh image"
    ourfsck_check "$img" "$data/ours-fresh.log" "our fsck-fat clean on the fresh image"

    # -- 2. mtools: a second, userspace FAT implementation writes to it
    if [ "$HAVE_MTOOLS" -eq 1 ]; then
        m="$WORK/$name.mtools.img"
        cp "$img" "$m"
        # MTOOLS_SKIP_CHECK: take the geometry from the boot sector, as the
        # kernel does, rather than insisting on a floppy drive's.
        export MTOOLS_SKIP_CHECK=1 MTOOLS_NO_VFAT=0
        check "mtools: created nested directories" \
            bash -c "mmd -i '$m' ::/adir && mmd -i '$m' ::/adir/deeper"
        check "mtools: wrote a file" mcopy -i "$m" "$data/hello.txt" ::/hello.txt
        check "mtools: wrote ${writekib} KiB, twice" \
            bash -c "mcopy -i '$m' '$data/big.bin' ::/big.bin && mcopy -i '$m' '$data/big.bin' ::/adir/deeper/copy.bin"
        check "mtools: wrote a long file name" mcopy -i "$m" "$data/lfn.txt" "::/A Long File Name.txt"
        rm -f "$data/back.bin" "$data/back-copy.bin" "$data/back-lfn.txt"
        check "mtools: read it back byte for byte" bash -c "
            mcopy -n -i '$m' ::/big.bin '$data/back.bin' &&
            mcopy -n -i '$m' ::/adir/deeper/copy.bin '$data/back-copy.bin' &&
            mcopy -n -i '$m' '::/A Long File Name.txt' '$data/back-lfn.txt' &&
            cmp -s '$data/big.bin' '$data/back.bin' &&
            cmp -s '$data/big.bin' '$data/back-copy.bin' &&
            cmp -s '$data/lfn.txt' '$data/back-lfn.txt'"
        dosfsck_check "$m" "$data/fsck-mtools.log" "fsck.fat clean after mtools wrote to it"
        ourfsck_check "$m" "$data/ours-mtools.log" "our fsck-fat clean after mtools wrote to it"
    elif [ "$REQUIRE_MTOOLS" -eq 1 ]; then
        bad "mtools stage required, but mtools is not installed"
    else
        skip "mtools stage (mtools not installed)"
    fi

    # -- 3. the kernel: loop mount read-write, write, unmount
    if [ "$CAN_MOUNT" -eq 1 ]; then
        k="$WORK/$name.kernel.img"
        mnt="$WORK/mnt/$name"
        cp "$img" "$k"
        mkdir -p "$mnt"
        if mount -o loop -t vfat "$k" "$mnt" 2> "$data/mount.log"; then
            ok "kernel mounted it read-write"
            check "kernel: created nested directories" mkdir -p "$mnt/adir/deeper"
            check "kernel: wrote a file" cp "$data/hello.txt" "$mnt/hello.txt"
            check "kernel: wrote ${writekib} KiB" cp "$data/big.bin" "$mnt/big.bin"
            check "kernel: copied it within the volume" cp "$mnt/big.bin" "$mnt/adir/deeper/copy.bin"
            check "kernel: wrote a long file name" cp "$data/lfn.txt" "$mnt/A Long File Name.txt"
            check "kernel: read it back byte for byte" cmp -s "$data/big.bin" "$mnt/adir/deeper/copy.bin"
            sync
            check "kernel: unmounted cleanly" umount "$mnt"
            dosfsck_check "$k" "$data/fsck-kernel.log" "fsck.fat clean after the kernel wrote to it"
            ourfsck_check "$k" "$data/ours-kernel.log" "our fsck-fat clean after the kernel wrote to it"
            if mount -o loop,ro -t vfat "$k" "$mnt" 2>> "$data/mount.log"; then
                check "kernel: remounted and read it back" \
                    bash -c "cmp -s '$data/hello.txt' '$mnt/hello.txt' && cmp -s '$data/lfn.txt' '$mnt/A Long File Name.txt'"
                umount "$mnt"
            else
                bad "kernel: remount failed"; sed 's/^/      /' "$data/mount.log"
            fi
        else
            bad "kernel mount failed"; sed 's/^/      /' "$data/mount.log"
        fi
    elif [ "$REQUIRE_MOUNT" -eq 1 ]; then
        bad "kernel stage required, but not running as root"
    else
        skip "kernel stage (needs root for a loop mount)"
    fi
done

hdr "result"
if [ "$FAILURES" -eq 0 ]; then
    echo -e "${GREEN}all configurations pass${RESET} ($SKIPS stage(s) skipped)"
else
    echo -e "${RED}$FAILURES check(s) failed${RESET} ($SKIPS stage(s) skipped)"
fi
exit $((FAILURES > 0))
