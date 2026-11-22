#!/bin/bash
# make-golden.sh — record reference filesystems from real mkfs.fat.
#
# The golden images in tests/golden are what dosfstools writes for a given size
# and set of options. `--invariant` pins the serial number and the timestamp, so
# two runs of mkfs.fat produce the same bytes and a byte comparison is possible
# at all.
#
# Runs on the machine it is invoked on, as an ordinary user — no ssh, no root.
# Needs a Linux host with dosfstools (mkfs.fat 4.2); macOS has newfs_msdos,
# which is a different program that writes different bytes. dev.g8.lo, where
# this used to ssh, is retired; record on a build VM and carry the result off:
#
#   SC_BUILD_OUT=tmp/golden.tar SC_BUILD_OUT_TO=tmp/golden.tar \
#     sc-build 'tests/make-golden.sh tmp/golden.tar'
#   tar -xf tmp/golden.tar -C tests/golden
#
# With no argument it writes straight into tests/golden.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
GOLDEN="$HERE/golden"
TAR="${1:-}"

command -v mkfs.fat >/dev/null || { echo "mkfs.fat (dosfstools) not installed" >&2; exit 2; }
mkfs.fat --help 2>&1 | head -1

W=$(mktemp -d "${TMPDIR:-/tmp}/mkfs-dos-golden.XXXXXX")
trap 'rm -rf "$W"' EXIT

# name:size-kib:mkfs.fat args. A `-m` names a file in tests/golden.
CASES=(
    "fat12-1440k:1440:"
    "fat12-8m:8192:"
    "fat12-32m-forced:32768:-F 12"
    "fat16-16m:16384:"
    "fat16-64m:65536:"
    "fat16-256m:262144:"
    "fat32-512m:524288:"
    "fat32-1g:1048576:"
    "fat16-64m-label:65536:-n TESTLABEL"
    "fat32-1g-label:1048576:-n ESP"
    "fat32-1g-4k:1048576:-S 4096"
    "fat16-64m-onefat:65536:-f 1"
    # -m (#10): a message with tabs, CRs, blank lines and a NUL; then one too
    # long for the boot code, cut at 447 bytes on FAT12/16 and 419 on FAT32.
    "fat12-1440k-msg:1440:-m $GOLDEN/msg-short.txt"
    "fat16-16m-msg-long:16384:-m $GOLDEN/msg-long.txt"
    "fat32-512m-msg:524288:-m $GOLDEN/msg-short.txt"
    "fat32-512m-msg-long:524288:-m $GOLDEN/msg-long.txt"
    # --mbr=y (#8): one case per partition type mkfs.fat picks — 0x01 FAT12,
    # 0x04 FAT16 under 65536 sectors, 0x06 within CHS, 0x0e past it or with
    # heads over 254, 0x0c FAT32 — and a long message cut at 439 by the table.
    "fat12-1440k-mbr:1440:--mbr=y"
    "fat16-16m-mbr:16384:--mbr=y"
    "fat16-64m-mbr:65536:--mbr=y"
    "fat16-64m-mbr-geom:65536:-g 255/63 --mbr=y"
    "fat16-1g-forced-mbr:1048576:-F 16 --mbr=y"
    "fat32-1g-mbr:1048576:--mbr=y"
    "fat16-16m-msg-long-mbr:16384:-m $GOLDEN/msg-long.txt --mbr=y"
    "fat32-512m-msg-long-mbr:524288:-m $GOLDEN/msg-long.txt --mbr=y"
    # -A / --variant atari (#11): GEMDOS's boot sector and geometry. FAT12 on
    # the floppy sector counts, FAT16 otherwise, FAT32 from 512 MiB; logical
    # sectors doubled past 65531 sectors (2 KiB at 64 MiB, 16 KiB at 512 MiB)
    # or past a FAT too big for its entries (-F 12 at 8 MiB); -S fixing it;
    # a label (root entry only); a fake MBR without 0xAA55.
    "atari-720k:720:-A"
    "atari-1440k:1440:-A"
    "atari-16m:16384:-A"
    "atari-64m:65536:-A"
    "atari-64m-label:65536:--variant atari -n GEMDOS"
    "atari-8m-f12:8192:-A -F 12"
    "atari-32m-s2048:32768:-A -S 2048"
    "atari-512m:524288:-A"
    "atari-16m-mbr:16384:-A --mbr=y"
    # -l FILE (#7): clusters marked bad in every FAT. FAT12 with a list in
    # every form strtoll takes (spaces, blank lines, +, CR, a leading 0, a
    # duplicate, no final newline) and an entry straddling FAT sectors 0/1
    # (block 186: clusters 341/342); FAT16; FAT32 (FSInfo left at
    # cluster_count - 1, as 4.2 leaves it); FAT32 on 4 KiB sectors and Atari
    # on 2 KiB logical sectors, inside 4.2's hard-vs-logical sector limit.
    "fat12-1440k-bad:1440:-l $GOLDEN/bad-1440k.txt"
    "fat16-16m-bad:16384:-l $GOLDEN/bad-16m.txt"
    "fat32-512m-bad:524288:-l $GOLDEN/bad-fat32-512m.txt"
    "fat32-1g-4k-bad:1048576:-S 4096 -l $GOLDEN/bad-fat32-1g-4k.txt"
    "atari-64m-bad:65536:-A -l $GOLDEN/bad-atari-64m.txt"
)

for case in "${CASES[@]}"; do
    IFS=: read -r name kib args <<< "$case"
    # Run inside $W so the dump names the image, not the build VM's paths.
    # shellcheck disable=SC2086 # args is a word list on purpose
    (
        cd "$W"
        truncate -s $((kib * 1024)) "$name.img"
        mkfs.fat --invariant -v $args "$name.img" > "$name.dump" 2>&1
        gzip -9 -f "$name.img"
    )
    echo "recorded $name"
done

# -C (#6): mkfs.fat creates the file itself (O_EXCL + ftruncate). Its
# output must be the truncated-file golden of the same size, byte for byte,
# which is what lets tests/cli_create.rs compare `mkfs-fat -C` against it.
for case in "fat12-1440k:1440" "fat16-16m:16384"; do
    IFS=: read -r name kib <<< "$case"
    (
        cd "$W"
        mkfs.fat --invariant -C "$name-create.img" "$kib" > /dev/null
        gzip -dc "$name.img.gz" | cmp - "$name-create.img"
        rm "$name-create.img"
    )
    echo "checked -C against $name"
done

# -c (#7): on an image file every block reads, so `mkfs.fat -c` writes the
# plain golden — the reference for `mkfs-fat -c` (tests/cli_badblocks.rs).
(
    cd "$W"
    truncate -s $((1440 * 1024)) check.img
    mkfs.fat --invariant -c check.img > /dev/null
    gzip -dc fat12-1440k.img.gz | cmp - check.img
    rm check.img
)
echo "checked -c against fat12-1440k"

# What 4.2 does with lists it refuses, or accepts and should not: exit
# status and output, into bad-probes.log (kept in the tar, not a golden).
# fsck.fat -n is run over every -l golden and over the FAT32 root-cluster
# case, which 4.2 formats with its root directory's cluster marked bad.
(
    cd "$W"
    probe() {   # probe NAME KIB LIST-CONTENT MKFS-ARGS...
        local name=$1 kib=$2 list=$3; shift 3
        printf '%b' "$list" > "$name.list"
        truncate -s $((kib * 1024)) "$name.img"
        echo "== $name: mkfs.fat $* -l <$(printf '%b' "$list" | tr '\n' ' ')>"
        set +e
        mkfs.fat --invariant "$@" -l "$name.list" "$name.img" 2>&1
        echo "exit $?"
        set -e
    }
    {
        probe before 1440 '16\n'
        probe behind 1440 '1440\n'
        probe minus-one 1440 '-1\n'
        probe minus-zero 1440 '-0\n'
        probe hex 1440 '0x20\n'
        probe word 1440 '17\nseventeen\n'
        probe two 1440 '17 18\n'
        probe range 1440 '99999999999999999999\n'
        probe empty 1440 ''
        probe nul 1440 '20\0junk\n'
        probe fat32-tail 524288 '524284\n'
        probe fat32-4k-limit 1048576 '131072\n' -S 4096
        probe fat32-root 524288 '1040\n'
        echo "== fsck.fat -n fat32-root"; set +e; fsck.fat -n fat32-root.img 2>&1; echo "exit $?"; set -e
        echo "== -c -l"; set +e; mkfs.fat -c -l before.list before.img 2>&1; echo "exit $?"; set -e
        for g in fat12-1440k-bad fat16-16m-bad fat32-512m-bad fat32-1g-4k-bad; do
            gzip -dc "$g.img.gz" > fsck.img
            echo "== fsck.fat -n $g"; set +e; fsck.fat -n fsck.img 2>&1; echo "exit $?"; set -e
        done
        rm -f fsck.img
    } > bad-probes.log
    rm -f ./*.list before.img behind.img minus-*.img hex.img word.img two.img \
        range.img empty.img nul.img fat32-*.img
    cat bad-probes.log
)

if [[ -n $TAR ]]; then
    mkdir -p "$(dirname "$TAR")"   # tmp/ is gitignored, so absent in a fresh clone
    tar -cf "$TAR" -C "$W" .
    echo "wrote $TAR"
else
    cp "$W"/*.img.gz "$W"/*.dump "$GOLDEN/"
    echo "now run: cargo test --release --test golden_compare"
fi
