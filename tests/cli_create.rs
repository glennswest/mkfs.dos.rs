//! `mkfs-fat -C` end to end (#6): the binary creates the image and formats
//! it, and the result is byte-identical to what `mkfs.fat 4.2` writes.
//!
//! `mkfs.fat -C` is `open(O_EXCL|O_CREAT)` + `ftruncate` and then the same
//! path as an existing file; `tests/make-golden.sh` checks that on real
//! `mkfs.fat` by comparing its `-C` output with the truncated-file golden.
//! So the golden for the size is the reference for `-C` too.

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

#[test]
fn create_writes_what_mkfs_fat_writes() {
    let dir = tempfile::tempdir().unwrap();
    for (name, blocks) in [("fat12-1440k", "1440"), ("fat16-16m", "16384")] {
        let path = dir.path().join(format!("{name}.img"));
        let p = path.to_str().unwrap();
        let out = mkfs(&["-C", "--invariant", p, blocks]);
        assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(std::fs::read(&path).unwrap() == golden(name), "{name}: differs from mkfs.fat");
    }
}

#[test]
fn create_refuses_an_existing_file_and_a_missing_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("there.img");
    std::fs::write(&path, vec![0x5au8; 1440 * 1024]).unwrap();
    let p = path.to_str().unwrap();

    let out = mkfs(&["-C", "--invariant", p, "1440"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already exists"));
    assert!(std::fs::read(&path).unwrap().iter().all(|&b| b == 0x5a), "the file was touched");

    let fresh = dir.path().join("fresh.img");
    let out = mkfs(&["-C", fresh.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Need intended size with -C."));
    assert!(!fresh.exists());
}
