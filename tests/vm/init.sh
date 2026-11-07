#!/bin/busybox sh
# The init of the kernel-verification VM (#4), run as PID 1 from the
# initramfs tests/vm/build-image.sh makes. It is verify-on-linux.sh's kernel
# stage (stage 3), as PID 1 under a real kernel instead of as root on a
# machine of ours. For each of the same eleven configurations, on a sparse
# image in tmpfs:
#
#   mkimage                           formatted by this crate, nothing else
#   fsck.fat -n, fsck-fat             clean, as we wrote it
#   the kernel loop-mounts it rw      mkdir -p, a file, 2 MiB (400 KiB on the
#                                     floppy), a copy within the volume, a
#                                     long file name; compared byte for byte
#   unmount; fsck.fat -n, fsck-fat    clean, after the kernel wrote
#   the kernel mounts it again ro     and reads back what it wrote
#
# Prints `VERIFY PASS` or `VERIFY FAIL <why>` on the serial console, then
# powers off. stormcentral's testhost boot watches for those lines.
/bin/busybox mount -t proc proc /proc
/bin/busybox --install -s /bin
export PATH=/bin:/sbin
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev
mkdir -p /mnt /work
mount -t tmpfs -o size=90% tmpfs /work
echo 1 > /proc/sys/kernel/printk 2>/dev/null

say() { echo "MKFS-DOS-VERIFY: $*"; }
fail() {
    say "FAIL: $*"
    dmesg | grep -iE 'fat|loop' | tail -20 | sed 's/^/MKFS-DOS-VERIFY dmesg: /'
    [ -s /work/out ] && tail -40 /work/out | sed 's/^/MKFS-DOS-VERIFY out: /'
    echo "VERIFY FAIL $*"
    sync; poweroff -f; sleep 30
}

say "kernel $(uname -r), $(cat /build-info 2>/dev/null)"
for m in $(cat /modules.order 2>/dev/null); do
    insmod "/modules/$m" || fail "insmod $m"
done
grep -qw vfat /proc/filesystems || fail "the kernel has no vfat"

# run what command... — the command's output to /work/out, failing with it.
run() {
    what=$1; shift
    "$@" >/work/out 2>&1 || fail "$what (exit $?): $(tail -3 /work/out | tr '\n' ' ')"
}

# fsck.fat from dosfstools (-n: never write) must find nothing to say, and
# our own fsck-fat must agree.
dosfsck() { # image what
    run "fsck.fat -n $2" fsck.fat -n "$1"
    if grep -qE "Dirty bit|Free cluster summary wrong|[Ll]ost|[Bb]ad |[Ee]rror|differ" /work/out; then
        fail "fsck.fat -n $2: $(head -3 /work/out | tr '\n' ' ')"
    fi
}
ourfsck() { run "fsck-fat $2" fsck-fat "$1"; }

# The same table as verify-on-linux.sh's CASES: name size write-kib [mkimage
# options]. Sizes are MiB unless they carry a 'k'.
fat_case() {
    name=$1 size=$2 writekib=$3; shift 3
    img=/work/$name.img data=/work/$name.data
    rm -rf "$img" "$data"; mkdir -p "$data"
    run "$name: mkimage $size $*" mkimage "$img" "$size" "$@"
    echo hello >"$data/hello.txt"
    echo "a long file name that needs a long directory entry" >"$data/lfn.txt"
    dd if=/dev/urandom of="$data/big.bin" bs=1K count="$writekib" 2>/dev/null \
        || fail "$name: making the test data"

    dosfsck "$img" "$name: as we wrote it"
    ourfsck "$img" "$name: as we wrote it"

    mount -t vfat -o loop "$img" /mnt 2>/work/out || fail "$name: mount: $(cat /work/out)"
    grep -q " /mnt vfat rw" /proc/mounts || fail "$name: not mounted vfat read-write"
    mkdir -p /mnt/adir/deeper || fail "$name: the kernel's mkdir -p"
    cp "$data/hello.txt" /mnt/hello.txt || fail "$name: the kernel's write of a file"
    cp "$data/big.bin" /mnt/big.bin || fail "$name: the kernel's ${writekib} KiB write"
    cp /mnt/big.bin /mnt/adir/deeper/copy.bin || fail "$name: the kernel's copy within the volume"
    cp "$data/lfn.txt" "/mnt/A Long File Name.txt" || fail "$name: the kernel's long file name"
    cmp -s "$data/big.bin" /mnt/adir/deeper/copy.bin || fail "$name: the kernel reads its copy wrong"
    sync
    umount /mnt || fail "$name: umount"

    dosfsck "$img" "$name: after the kernel wrote"
    ourfsck "$img" "$name: after the kernel wrote"

    mount -t vfat -o loop,ro "$img" /mnt 2>/work/out || fail "$name: remount: $(cat /work/out)"
    cmp -s "$data/hello.txt" /mnt/hello.txt || fail "$name: hello.txt reads back wrong"
    cmp -s "$data/big.bin" /mnt/big.bin || fail "$name: big.bin reads back wrong"
    cmp -s "$data/big.bin" /mnt/adir/deeper/copy.bin || fail "$name: the copy reads back wrong"
    cmp -s "$data/lfn.txt" "/mnt/A Long File Name.txt" || fail "$name: the long file name reads back wrong"
    umount /mnt || fail "$name: umount (ro)"

    say "$name ($size $*): fsck clean; the kernel mounted rw, wrote ${writekib} KiB x2, mkdir -p, a long name; fsck.fat and fsck-fat clean after; remounted and read it all back"
    rm -rf "$img" "$data"
    PASSED=$((PASSED + 1))
}

PASSED=0
fat_case fat12-8m 8 2048
fat_case fat12-32m-forced 32 2048 12
fat_case fat16-64m 64 2048
fat_case fat16-256m 256 2048 label=SIXTEEN
fat_case fat16-64m-onefat 64 2048 fats=1
fat_case fat16-64m-noalign 64 2048 noalign
fat_case fat32-512m 512 2048
fat_case fat32-1g 1024 2048 label=ESP
fat_case fat32-1g-4k 1024 2048 sector=4096
fat_case fat32-64m-forced 64 2048 32
# A real 1.44 MB floppy, where the historical parameters apply.
fat_case fat12-1440k 1440k 400

say "all $PASSED configurations pass the kernel stage"
echo "VERIFY PASS"
sync; poweroff -f; sleep 30
