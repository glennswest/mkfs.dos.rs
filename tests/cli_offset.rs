//! `mkfs-fat --offset` end to end (#9): the binary parses the offset as
//! `mkfs.fat 4.2` does and writes, at the offset, what 4.2 writes — compared
//! byte for byte with goldens `tests/make-golden.sh` recorded from it.

use std::io::Read;
use std::path::Path;
use std::process::Command;

fn golden(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.img.gz"));
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
        .read_to_end(&mut out)
        .unwrap();
    out
}

fn mkfs(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mkfs-fat"))
        .args(args)
        .output()
        .expect("run mkfs-fat")
}

fn image(dir: &Path, name: &str, size: u64) -> String {
    let path = dir.join(format!("{name}.img"));
    std::fs::File::create(&path).unwrap().set_len(size).unwrap();
    path.to_str().unwrap().to_string()
}

const MIB: u64 = 1 << 20;

#[test]
fn offset_writes_what_mkfs_fat_writes() {
    let dir = tempfile::tempdir().unwrap();
    let cases: &[(&str, u64, &[&str])] = &[
        ("fat16-64m-off2048", 64 * MIB, &["--offset", "2048"]),
        ("fat12-1440k-off63", 1440 * 1024, &["--offset=63"]),
        ("fat32-1g-4k-off256", 1024 * MIB, &["-S", "4096", "--offset", "256"]),
        ("fat16-64m-off2048-mbr", 64 * MIB, &["--offset", "2048", "--mbr=y"]),
        ("fat16-64m-off2048-h", 64 * MIB, &["--offset", "2048", "-H", "2048"]),
        // -c through the window: 4.2 dies here (`seek failed during testing
        // for blocks`); an image file reads, so it is the plain golden.
        ("fat16-64m-off2048", 64 * MIB, &["-c", "--offset", "2048"]),
    ];
    for (name, size, args) in cases {
        let p = image(dir.path(), name, *size);
        let mut argv = vec!["--invariant"];
        argv.extend_from_slice(args);
        argv.push(&p);
        let out = mkfs(&argv);
        assert!(out.status.success(), "{name} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(std::fs::read(&p).unwrap() == golden(name), "{name} {args:?}: differs from mkfs.fat");
        std::fs::remove_file(&p).unwrap();
    }
}

#[test]
fn offset_with_blocks_and_create() {
    let dir = tempfile::tempdir().unwrap();
    // A hex offset and a BLOCKS count inside the rest, with 4.2's warning.
    let p = image(dir.path(), "blocks", 64 * MIB);
    let out = mkfs(&["--invariant", "--offset", "0x800", &p, "16384"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr)
        .contains("Warning: block count mismatch: found 64512 but assuming 16384."));
    assert!(std::fs::read(&p).unwrap() == golden("fat16-64m-off0x800-blocks"));

    // BLOCKS alone: the device, not BLOCKS, decides the floppy defaults.
    let p = image(dir.path(), "two", 2 * MIB);
    let out = mkfs(&["--invariant", &p, "1440"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(std::fs::read(&p).unwrap() == golden("fat12-2m-blocks1440"));

    // -C makes the file offset + BLOCKS x 1024 long, as 4.2's ftruncate does;
    // make-golden.sh checked 4.2's own -C --offset against this golden.
    let p = dir.path().join("create.img");
    let p = p.to_str().unwrap();
    let out = mkfs(&["-C", "--invariant", "--offset", "2048", p, "16384"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(std::fs::read(p).unwrap() == golden("fat16-17m-off2048"));
}

#[test]
fn offset_refusals_leave_the_device_alone() {
    let dir = tempfile::tempdir().unwrap();
    let cases: &[(u64, &[&str], &str)] = &[
        (1440 * 1024, &["--offset", "-1"], "FAT offset must be between 0 and 9223372036854775807: -1"),
        (1440 * 1024, &["--offset", "08"], "Bad number for offset : 08"),
        (1440 * 1024, &["--offset", "8x"], "Bad number for offset : 8x"),
        (1440 * 1024, &["--offset", "2880"], "less than the offset"),
        // 4.2 writes this one: measured from 8 x 512, written at 8 x 1024,
        // so the volume ends 4 KiB past the device.
        (64 * MIB, &["-A", "--offset", "8"], "give -S 1024"),
    ];
    for (size, args, want) in cases {
        let p = image(dir.path(), "refused", *size);
        std::fs::write(&p, vec![0x5au8; *size as usize]).unwrap();
        let mut argv = vec!["--invariant"];
        argv.extend_from_slice(args);
        argv.push(&p);
        let out = mkfs(&argv);
        assert!(!out.status.success(), "{args:?} was accepted");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(want), "{args:?}: {err}");
        assert!(std::fs::read(&p).unwrap().iter().all(|&b| b == 0x5a), "{args:?}: the device was touched");
    }
}
