//! `mkfs-fat --codepage` end to end (#11). With `mkfs.fat 4.2` the code page
//! changes no byte: an ASCII label is the same in all four, and every
//! non-ASCII one is refused — "below 0x20" when the code page has the
//! character (4.2's signed-char test), "Error when processing label" when it
//! does not. `tests/make-golden.sh` `cmp`s 4.2's `--codepage N -n TESTLABEL`
//! against the `fat16-64m-label` golden and logs its refusals in
//! `codepage-probes.log`; this holds `mkfs-fat` to the same.

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
fn an_ascii_label_is_the_same_in_every_codepage() {
    let dir = tempfile::tempdir().unwrap();
    let want = golden("fat16-64m-label");
    let cases: &[&[&str]] = &[
        &["--codepage", "437"],
        &["--codepage=850"],
        &["--codepage", "852"],
        &["--codepage", "866"],
        // The first --codepage decides; a later one need only be a number.
        &["--codepage", "437", "--codepage", "9999"],
    ];
    for args in cases {
        let p = image(dir.path(), "cp", 64 * MIB);
        let mut argv = vec!["--invariant", "-n", "TESTLABEL"];
        argv.extend_from_slice(args);
        argv.push(&p);
        let out = mkfs(&argv);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(std::fs::read(&p).unwrap() == want, "{args:?}: differs from mkfs.fat");
        std::fs::remove_file(&p).unwrap();
    }
}

#[test]
fn refusals_are_mkfs_fat_s_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cases: &[(&[&str], &str)] = &[
        (&["-n", "ÄBC", "--codepage", "437"], "Labels with characters below 0x20 are not allowed"),
        (&["-n", "äbc", "--codepage", "850"], "Labels with characters below 0x20 are not allowed"),
        (&["-n", "ÄBC"], "Labels with characters below 0x20 are not allowed"),
        (&["-n", "ŁÓDŹ", "--codepage", "852"], "Labels with characters below 0x20 are not allowed"),
        (&["-n", "Ж", "--codepage", "866"], "Labels with characters below 0x20 are not allowed"),
        (&["-n", "Ж"], "Error when processing label"),
        (&["-n", "ÄBC", "--codepage", "866"], "Error when processing label"),
        (&["--codepage", "9999"], "Cannot initialize codepage 9999"),
        (&["--codepage", "9999", "--codepage", "437"], "Cannot initialize codepage 9999"),
        (&["--codepage", "abc"], "Invalid codepage : abc"),
        (&["--codepage", "-5"], "Invalid codepage : -5"),
    ];
    for (args, want) in cases {
        let p = image(dir.path(), "cp", 16 * MIB);
        let mut argv = vec!["--invariant"];
        argv.extend_from_slice(args);
        argv.push(&p);
        let out = mkfs(&argv);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {err}");
        assert!(err.contains(want), "{args:?}: wanted {want:?}, got {err}");
        assert!(
            std::fs::read(&p).unwrap().iter().all(|&b| b == 0),
            "{args:?}: wrote to the image"
        );
        std::fs::remove_file(&p).unwrap();
    }
}
