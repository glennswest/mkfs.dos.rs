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

use mkfs_dos::{format, FatType, MemDevice, Params};

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
        name: "fat12-8m",
        size: 8 * MIB,
        params: || Params::new().invariant(),
    },
    // 32 MiB would be FAT16 left to itself; `-F 12` forces the narrower entry
    // and the search answers with 16 KiB clusters to keep the count under 4085.
    Case {
        name: "fat12-32m-forced",
        size: 32 * MIB,
        params: || Params::new().invariant().fat_type(FatType::Fat12),
    },
    Case {
        name: "fat16-16m",
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
    // `-m FILE` (#10): the boot code's message replaced. The short one has
    // tabs, a CR, a blank line and a NUL, each of which `mkfs.fat` rewrites;
    // the long one overruns the area and is cut at 447 bytes on FAT12/16 and
    // at 419 on FAT32, whose boot code also moves 28 bytes and is re-pointed.
    Case {
        name: "fat12-1440k-msg",
        size: 1440 * KIB,
        params: || Params::new().invariant().boot_message(MSG_SHORT),
    },
    Case {
        name: "fat16-16m-msg-long",
        size: 16 * MIB,
        params: || Params::new().invariant().boot_message(MSG_LONG),
    },
    Case {
        name: "fat32-512m-msg",
        size: 512 * MIB,
        params: || Params::new().invariant().boot_message(MSG_SHORT),
    },
    Case {
        name: "fat32-512m-msg-long",
        size: 512 * MIB,
        params: || Params::new().invariant().boot_message(MSG_LONG),
    },
    // `--mbr=y` (#8): a partition table in the boot sector, disk signature
    // the invariant serial. One case per partition type `mkfs.fat` picks:
    // 0x01 FAT12; 0x04 FAT16 under 65536 sectors; 0x06 within CHS reach;
    // 0x0e past it, and with 255 heads; 0x0c FAT32. Then a long message cut
    // at offset 439, where the table begins.
    Case {
        name: "fat12-1440k-mbr",
        size: 1440 * KIB,
        params: || Params::new().invariant().mbr(),
    },
    Case {
        name: "fat16-16m-mbr",
        size: 16 * MIB,
        params: || Params::new().invariant().mbr(),
    },
    Case {
        name: "fat16-64m-mbr",
        size: 64 * MIB,
        params: || Params::new().invariant().mbr(),
    },
    Case {
        name: "fat16-64m-mbr-geom",
        size: 64 * MIB,
        params: || Params::new().invariant().geometry(255, 63).mbr(),
    },
    Case {
        name: "fat16-1g-forced-mbr",
        size: 1024 * MIB,
        params: || Params::new().invariant().fat_type(FatType::Fat16).mbr(),
    },
    Case {
        name: "fat32-1g-mbr",
        size: 1024 * MIB,
        params: || Params::new().invariant().mbr(),
    },
    Case {
        name: "fat16-16m-msg-long-mbr",
        size: 16 * MIB,
        params: || Params::new().invariant().boot_message(MSG_LONG).mbr(),
    },
    Case {
        name: "fat32-512m-msg-long-mbr",
        size: 512 * MIB,
        params: || Params::new().invariant().boot_message(MSG_LONG).mbr(),
    },
    // `-A` / `--variant atari` (#11): the GEMDOS boot sector (68000 branch,
    // `kdosf` and a 24-bit serial, no extended boot record, no 0xAA55) and
    // its geometry — FAT12 only on the floppy sector counts, two-sector
    // clusters, logical sectors doubled past 65531 sectors (2 KiB at 64 MiB,
    // 16 KiB on the 512 MiB FAT32) or past a FAT too big for its entries
    // (`-F 12` at 8 MiB), held by `-S`; a label in the root only; a fake MBR.
    Case {
        name: "atari-720k",
        size: 720 * KIB,
        params: || Params::new().invariant().atari(),
    },
    Case {
        name: "atari-1440k",
        size: 1440 * KIB,
        params: || Params::new().invariant().atari(),
    },
    Case {
        name: "atari-16m",
        size: 16 * MIB,
        params: || Params::new().invariant().atari(),
    },
    Case {
        name: "atari-64m",
        size: 64 * MIB,
        params: || Params::new().invariant().atari(),
    },
    Case {
        name: "atari-64m-label",
        size: 64 * MIB,
        params: || Params::new().invariant().atari().label("GEMDOS"),
    },
    Case {
        name: "atari-8m-f12",
        size: 8 * MIB,
        params: || Params::new().invariant().atari().fat_type(FatType::Fat12),
    },
    Case {
        name: "atari-32m-s2048",
        size: 32 * MIB,
        params: || Params::new().invariant().atari().sector_size(2048),
    },
    Case {
        name: "atari-512m",
        size: 512 * MIB,
        params: || Params::new().invariant().atari(),
    },
    Case {
        name: "atari-16m-mbr",
        size: 16 * MIB,
        params: || Params::new().invariant().atari().mbr(),
    },
];

/// The message files `tests/make-golden.sh` hands `mkfs.fat -m`.
const MSG_SHORT: &[u8] = include_bytes!("golden/msg-short.txt");
const MSG_LONG: &[u8] = include_bytes!("golden/msg-long.txt");

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
