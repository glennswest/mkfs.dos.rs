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

# name:size-kib:mkfs.fat args[:BLOCKS]. A `-m` names a file in tests/golden.
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
    # --offset (#9): the filesystem from sector N on, hidden sectors left at
    # 0 on an image file. The defaults establish_params() takes from the
    # device — floppy match, CHS, FAT32 from 512 MiB — see the whole file
    # (1440k-off63 keeps the floppy parameters; 520m-off32768 is FAT32 with
    # 504 MiB left). 4 KiB sectors count the offset in 4 KiB; a fake MBR
    # goes at the offset; -h wins; hex with a BLOCKS count inside the rest;
    # Atari where the sector size does not grow (a floppy, -S 2048). And
    # BLOCKS alone on a bigger file: the device decides, not BLOCKS (no
    # floppy parameters for 1440 blocks of a 2 MiB file).
    "fat12-1440k-off63:1440:--offset 63"
    "fat16-64m-off2048:65536:--offset 2048"
    "fat16-17m-off2048:17408:--offset 2048"
    "fat32-1g-off2048:1048576:--offset 2048"
    "fat32-520m-off32768:532480:--offset 32768"
    "fat32-1g-4k-off256:1048576:-S 4096 --offset 256"
    "fat16-64m-off2048-mbr:65536:--offset 2048 --mbr=y"
    "fat16-64m-off2048-h:65536:--offset 2048 -h 2048"
    "fat16-64m-off0x800-blocks:65536:--offset 0x800:16384"
    "fat12-2m-blocks1440:2048::1440"
    "atari-1440k-off10:1440:-A --offset 10"
    "atari-32m-s2048-off16:32768:-A -S 2048 --offset 16"
)

for case in "${CASES[@]}"; do
    IFS=: read -r name kib args blocks <<< "$case"
    # Run inside $W so the dump names the image, not the build VM's paths.
    # shellcheck disable=SC2086 # args and blocks are word lists on purpose
    (
        cd "$W"
        truncate -s $((kib * 1024)) "$name.img"
        mkfs.fat --invariant -v $args "$name.img" $blocks > "$name.dump" 2>&1
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

# -C --offset (#9): the file is offset + BLOCKS x 1024 bytes, formatted from
# the offset — the 17 MiB golden with 16 MiB of filesystem at sector 2048.
(
    cd "$W"
    mkfs.fat --invariant -C --offset 2048 off-create.img 16384 > /dev/null
    gzip -dc fat16-17m-off2048.img.gz | cmp - off-create.img
    rm off-create.img
)
echo "checked -C --offset against fat16-17m-off2048"

# --codepage (#11): 4.2 refuses every non-ASCII label, so an ASCII label is
# the same bytes in every code page — the -n TESTLABEL golden, byte for byte.
for cp in 437 850 852 866; do
    (
        cd "$W"
        truncate -s $((65536 * 1024)) cp.img
        mkfs.fat --invariant --codepage "$cp" -n TESTLABEL cp.img > /dev/null
        gzip -dc fat16-64m-label.img.gz | cmp - cp.img
        rm cp.img
    )
    echo "checked --codepage $cp against fat16-64m-label"
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
    # --offset (#9): what 4.2 refuses, -c with an offset, and Atari when the
    # logical sector grows (the file length after it says where it wrote).
    # fsck.fat -n over the filesystem cut out of each offset golden.
    oprobe() {   # oprobe NAME KIB MKFS-ARGS...
        local name=$1 kib=$2; shift 2
        truncate -s $((kib * 1024)) "$name.img"
        echo "== $name: mkfs.fat $*"
        set +e
        mkfs.fat --invariant "$@" "$name.img" 2>&1
        echo "exit $? size $(stat -c %s "$name.img")"
        set -e
        rm -f "$name.img"
    }
    {
        oprobe past-end 1440 --offset 2880
        oprobe at-end 1440 --offset 2879
        oprobe negative 1440 --offset -1
        oprobe minus-zero 1440 --offset -0
        oprobe octal-bad 1440 --offset 08
        oprobe octal 1440 --offset 010
        oprobe space 1440 --offset ' 8'
        oprobe trailing 1440 --offset 8x
        oprobe huge 1440 --offset 99999999999999999999
        oprobe check 65536 -c --offset 2048
        oprobe atari-grow 65536 -A --offset 8
        oprobe atari-hidden-16 65536 -A -S 2048 --offset 70000
        for g in fat12-1440k-off63:63:512 fat16-64m-off2048:2048:512 \
                 fat32-1g-off2048:2048:512 fat32-1g-4k-off256:256:4096 \
                 fat32-520m-off32768:32768:512 atari-32m-s2048-off16:16:2048; do
            IFS=: read -r n sec ss <<< "$g"
            gzip -dc "$n.img.gz" | tail -c +$((sec * ss + 1)) > fsck.img
            echo "== fsck.fat -n $n (from byte $((sec * ss)))"; set +e; fsck.fat -n fsck.img 2>&1; echo "exit $?"; set -e
        done
        rm -f fsck.img
    } > offset-probes.log
    # --codepage (#11): what 4.2 says to a non-ASCII label in each code page,
    # to a code page iconv lacks, and to one that is not a number; whether it
    # wrote anything (cmp against an untouched file). In a UTF-8 locale, as
    # mkfs.fat converts from the locale's codeset.
    cprobe() {   # cprobe NAME MKFS-ARGS...
        local name=$1; shift
        truncate -s $((16384 * 1024)) "$name.img"
        echo "== $name: mkfs.fat $*"
        set +e
        LC_ALL=C.UTF-8 mkfs.fat --invariant "$@" "$name.img" 2>&1 | grep -v '^  \|^Usage\|^Create FAT\|^up to BLOCKS\|^created with\|^Options:\|^Report bugs\|^$'
        echo "exit ${PIPESTATUS[0]} $(cmp -s "$name.img" <(head -c $((16384 * 1024)) /dev/zero) && echo untouched || echo written)"
        set -e
        rm -f "$name.img"
    }
    {
        cprobe cp437-upper -n ÄBC --codepage 437
        cprobe cp850-lower -n äbc --codepage 850
        cprobe default-upper -n ÄBC
        cprobe cp852 -n ŁÓDŹ --codepage 852
        cprobe cp866 -n Ж --codepage 866
        cprobe default-cyrillic -n Ж
        cprobe cp866-latin -n ÄBC --codepage 866
        cprobe unknown --codepage 9999
        cprobe unknown-first --codepage 9999 --codepage 437
        cprobe known-first --codepage 437 --codepage 9999 -n TESTLABEL
        cprobe word --codepage abc
        cprobe negative --codepage -5
    } > codepage-probes.log
    cat codepage-probes.log
    cat offset-probes.log
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
