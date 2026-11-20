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
)

for case in "${CASES[@]}"; do
    IFS=: read -r name kib args <<< "$case"
    truncate -s $((kib * 1024)) "$W/$name.img"
    # shellcheck disable=SC2086 # args is a word list on purpose
    mkfs.fat --invariant -v $args "$W/$name.img" > "$W/$name.dump" 2>&1
    gzip -9 -f "$W/$name.img"
    echo "recorded $name"
done

if [[ -n $TAR ]]; then
    mkdir -p "$(dirname "$TAR")"   # tmp/ is gitignored, so absent in a fresh clone
    tar -cf "$TAR" -C "$W" .
    echo "wrote $TAR"
else
    cp "$W"/*.img.gz "$W"/*.dump "$GOLDEN/"
    echo "now run: cargo test --release --test golden_compare"
fi
