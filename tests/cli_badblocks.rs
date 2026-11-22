//! `mkfs-fat -l FILE` and `-c` end to end (#7), against what `mkfs.fat 4.2`
//! writes: the `-l` golden for the list, and for `-c` on an image file —
//! where every block reads — the plain golden (`tests/make-golden.sh` checks
//! real `mkfs.fat -c` gives the same bytes).

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

fn blank(dir: &Path, name: &str, kib: u64) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::File::create(&path).unwrap().set_len(kib * 1024).unwrap();
    path
}

#[test]
fn a_bad_block_list_writes_what_mkfs_fat_writes() {
    let dir = tempfile::tempdir().unwrap();
    let img = blank(dir.path(), "bad.img", 1440);
    let list = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/bad-1440k.txt");
    let out = mkfs(&["--invariant", "-l", list.to_str().unwrap(), img.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // Seven lines with a number, the duplicate counted, as mkfs.fat counts.
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("7 bad blocks\n"));
    assert!(std::fs::read(&img).unwrap() == golden("fat12-1440k-bad"));
}

#[test]
fn check_on_an_image_file_writes_the_plain_golden() {
    let dir = tempfile::tempdir().unwrap();
    let img = blank(dir.path(), "check.img", 1440);
    let out = mkfs(&["--invariant", "-c", img.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("bad block"));
    assert!(std::fs::read(&img).unwrap() == golden("fat12-1440k"));
}

#[test]
fn refused_lists_leave_the_image_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("keep.img");
    std::fs::write(&img, vec![0x5au8; 1440 * 1024]).unwrap();
    let i = img.to_str().unwrap();
    for (content, says) in [
        ("16\n", "before data area"),
        ("1440\n", "behind end of filesystem"),
        ("17\nseventeen\n", "line 2"),
        ("-1\n", "negative"),
    ] {
        let list = dir.path().join("list");
        std::fs::write(&list, content).unwrap();
        let out = mkfs(&["--invariant", "-l", list.to_str().unwrap(), i]);
        assert!(!out.status.success(), "{content:?} was accepted");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(says), "{content:?}: {err}");
        assert!(std::fs::read(&img).unwrap().iter().all(|&b| b == 0x5a), "{content:?} wrote");
    }
    let list = dir.path().join("list");
    let out = mkfs(&["-c", "-l", list.to_str().unwrap(), i]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("-c and -l are incompatible"));
}
