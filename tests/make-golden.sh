#!/bin/bash
# make-golden.sh — record reference filesystems from real mkfs.fat.
#
# The golden images in tests/golden are what dosfstools writes for a given size
# and set of options. `--invariant` pins the serial number and the timestamp, so
# two runs of mkfs.fat produce the same bytes and a byte comparison is possible
# at all.
#
# Needs a Linux host with dosfstools; macOS has newfs_msdos, which is a
# different program that writes different bytes.
#
#   ./tests/make-golden.sh [user@host]
#
# Defaults to root@dev.g8.lo.

set -euo pipefail

HOST="${1:-root@dev.g8.lo}"
REMOTE_DIR=/root/fatgold
LOCAL_DIR="$(cd "$(dirname "$0")" && pwd)/golden"

# name:size-kib:mkfs.fat args
CASES=(
    "fat12-1440k:1440:"
    "fat12-16m:16384:"
    "fat16-64m:65536:"
    "fat16-256m:262144:"
    "fat32-512m:524288:"
    "fat32-1g:1048576:"
    "fat16-64m-label:65536:-n TESTLABEL"
    "fat32-1g-label:1048576:-n ESP"
    "fat32-1g-4k:1048576:-S 4096"
    "fat16-64m-onefat:65536:-f 1"
)

echo "recording golden images on $HOST"
{
    echo "set -euo pipefail"
    echo "rm -rf $REMOTE_DIR && mkdir -p $REMOTE_DIR && cd $REMOTE_DIR"
    echo "mkfs.fat --help >/dev/null 2>&1 || { echo 'dosfstools not installed' >&2; exit 1; }"
    for case in "${CASES[@]}"; do
        IFS=: read -r name kib args <<< "$case"
        echo "truncate -s \$((${kib}*1024)) ${name}.img"
        echo "mkfs.fat --invariant -v ${args} ${name}.img > ${name}.dump 2>&1"
        echo "gzip -9 -k -f ${name}.img"
    done
} | ssh "$HOST" bash -s

mkdir -p "$LOCAL_DIR"
scp -q "$HOST:$REMOTE_DIR/*.img.gz" "$HOST:$REMOTE_DIR/*.dump" "$LOCAL_DIR/"
ls -la "$LOCAL_DIR"
echo
echo "now run: cargo test --release --test golden_compare"
