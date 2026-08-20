//! Byte-for-byte comparison against real `mkfs.fat`.
//!
//! Each golden image was produced by dosfstools 4.2 with `--invariant`, which
//! pins the serial number and the timestamp and so makes its output
//! reproducible. Our formatter is handed the same size and the same options,
//! and the two images must be identical — every byte, metadata and data area
//! alike.
//!
//! This is the test the crate exists to pass. A geometry that merely *works* is
//! easy; one that agrees with what every other tool would have written is the
//! only kind worth shipping, because a difference is either a bug here or a
//! reason, and a byte comparison is what forces the question.
//!
//! Regenerate the goldens with `tests/make-golden.sh`.

use std::io::Read;
use std::path::Path;

use mkfs_dos::{format, MemDevice, Params};

/// A golden image and the parameters that should reproduce it.
struct Case {
    name: &'static str,
    size: u64,
    params: fn() -> Params,
}

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

const CASES: &[Case] = &[
    // A 1.44 MB floppy: the one size where the historical parameters — a 0xf0
    // media byte, 18 sectors a track, 224 root entries — still apply.
    Case {
        name: "fat12-1440k",
        size: 1440 * KIB,
        params: || Params::new().invariant(),
    },
    Case {
        name: "fat12-16m",
        size: 16 * MIB,
        params: || Params::new().invariant(),
    },
    Case {
        name: "fat16-64m",
        size: 64 * MIB,
        params: || Params::new().invariant(),
    },
    Case {
        name: "fat16-256m",
        size: 256 * MIB,
        params: || Params::new().invariant(),
    },
    // 512 MiB is the threshold where `mkfs.fat` switches to FAT32 unasked.
    Case {
        name: "fat32-512m",
        size: 512 * MIB,
        params: || Params::new().invariant(),
    },
    Case {
        name: "fat32-1g",
        size: 1024 * MIB,
        params: || Params::new().invariant(),
    },
    Case {
        name: "fat16-64m-label",
        size: 64 * MIB,
        params: || Params::new().invariant().label("TESTLABEL"),
    },
    Case {
        name: "fat32-1g-label",
        size: 1024 * MIB,
        params: || Params::new().invariant().label("ESP"),
    },
    // A 4 KiB sector size changes every offset in the filesystem.
    Case {
        name: "fat32-1g-4k",
        size: 1024 * MIB,
        params: || Params::new().invariant().sector_size(4096),
    },
    // One FAT: legal, half the redundancy, and a different cut-off correction
    // in the geometry search.
    Case {
        name: "fat16-64m-onefat",
        size: 64 * MIB,
        params: || Params::new().invariant().num_fats(1),
    },
];

#[tokio::test]
async fn our_images_are_identical_to_dosfstools() {
    let mut failures = Vec::new();

    for case in CASES {
        let golden = match load_golden(case.name) {
            Some(g) => g,
            None => {
                failures.push(format!("{}: golden image missing", case.name));
                continue;
            }
        };
        assert_eq!(
            golden.len() as u64,
            case.size,
            "{}: golden is {} bytes, case says {}",
            case.name,
            golden.len(),
            case.size
        );

        let device = MemDevice::new(case.size);
        let report = match format(&device, &(case.params)()).await {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{}: format failed: {e}", case.name));
                continue;
            }
        };
        let ours = device.to_vec();

        if let Some(diff) = first_difference(&golden, &ours) {
            failures.push(format!("{}: {} [{}]", case.name, diff, report));
        }
    }

    assert!(
        failures.is_empty(),
        "images differ from dosfstools:\n{}",
        failures.join("\n")
    );
}

/// Where two images first differ, described well enough to act on.
///
/// A byte offset alone sends you to a hex dump; the region it falls in and the
/// bytes on both sides usually identify the field on their own.
fn first_difference(golden: &[u8], ours: &[u8]) -> Option<String> {
    if golden.len() != ours.len() {
        return Some(format!(
            "length differs: golden {} bytes, ours {}",
            golden.len(),
            ours.len()
        ));
    }
    let at = golden.iter().zip(ours).position(|(a, b)| a != b)?;

    let differing = golden
        .iter()
        .zip(ours)
        .filter(|(a, b)| a != b)
        .count();

    let start = at.saturating_sub(8);
    let end = (at + 24).min(golden.len());
    Some(format!(
        "{differing} bytes differ, first at {at} ({at:#x}, sector {}, offset {} in it)\n  golden {:02x?}\n  ours   {:02x?}",
        at / 512,
        at % 512,
        &golden[start..end],
        &ours[start..end],
    ))
}

/// Decompress a golden image.
fn load_golden(name: &str) -> Option<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.img.gz"));
    let file = std::fs::File::open(path).ok()?;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(file)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}
