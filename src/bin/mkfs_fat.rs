//! `mkfs.fat` — create a FAT12, FAT16 or FAT32 filesystem.
//!
//! Flags follow dosfstools' `mkfs.fat` where they mean the same thing, so
//! muscle memory and existing scripts carry over.

use std::io::{Read, Write};

use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};

use mkfs_dos::device::{BlockDevice, FileDevice, OffsetDevice};
use mkfs_dos::format::format_with;
use mkfs_dos::layout::Geometry;
use mkfs_dos::params::{FatType, Params, Variant, OLDGEMDOS_MAX_SECTORS};
use mkfs_dos::BootMessage;

#[derive(Parser, Debug)]
#[command(
    name = "mkfs.fat",
    about = "Create a FAT12/FAT16/FAT32 filesystem",
    version
)]
struct Args {
    /// Device or image file to format.
    device: String,

    /// Size in 1024-byte blocks. Defaults to the whole device.
    blocks: Option<u64>,

    /// FAT width: 12, 16 or 32. Defaults to whatever the size calls for.
    #[arg(short = 'F', long = "fat")]
    fat_type: Option<String>,

    /// Logical sector size in bytes. Never smaller than the device's.
    #[arg(short = 'S', long)]
    sector_size: Option<u32>,

    /// Sectors per cluster, a power of two from 1 to 128.
    #[arg(short = 's', long)]
    sectors_per_cluster: Option<u8>,

    /// Reserved sectors. Defaults to 1 on FAT12/16 and 32 on FAT32.
    #[arg(short = 'R', long)]
    reserved: Option<u16>,

    /// Number of FATs.
    #[arg(short = 'f', long, default_value_t = 2)]
    num_fats: u8,

    /// Root directory entries. FAT12/16 only.
    #[arg(short = 'r', long)]
    root_entries: Option<u16>,

    /// Volume label, at most 11 characters.
    #[arg(short = 'n', long)]
    label: Option<String>,

    /// Volume serial number, in hex.
    #[arg(short = 'i', long)]
    volume_id: Option<String>,

    /// Media descriptor byte, in hex.
    #[arg(short = 'M', long)]
    media: Option<String>,

    /// Sectors before the start of this filesystem.
    #[arg(short = 'H', long)]
    hidden: Option<u32>,

    /// Geometry as heads/sectors-per-track, e.g. `255/63`.
    #[arg(short = 'g', long)]
    geometry: Option<String>,

    /// BIOS drive number, in hex.
    #[arg(short = 'D', long)]
    drive_number: Option<String>,

    /// Read the boot-sector message from FILE (`-` for standard input). It is
    /// what the volume prints if a machine tries to boot from it.
    #[arg(short = 'm', long = "message-file", value_name = "FILE")]
    message_file: Option<String>,

    /// Read every 1024-byte block of the target before formatting and mark
    /// the clusters holding unreadable ones bad. A bad block before the data
    /// area stops the format.
    #[arg(short = 'c', long = "check")]
    check: bool,

    /// Read a list of bad blocks from FILE: one 1024-byte block number per
    /// line, counted from the start of the filesystem. The clusters holding
    /// them are marked bad in every FAT.
    #[arg(short = 'l', long = "bad-blocks-file", value_name = "FILE")]
    bad_blocks_file: Option<String>,

    /// Sector holding the backup boot sector. FAT32 only.
    #[arg(short = 'b', long)]
    backup_boot: Option<u16>,

    /// Do not align structures to cluster boundaries.
    #[arg(short = 'a', long)]
    no_align: bool,

    /// Treat the target as a fixed disk, so the floppy defaults never apply.
    /// A plain image file is treated as removable, exactly as `mkfs.fat` does.
    #[arg(long)]
    fixed: bool,

    /// Write a (fake) MBR partition table into the boot sector: one active
    /// partition spanning the whole volume, so Windows recognises a whole-disk
    /// image. `y` (what a bare `--mbr` means), `n`, or `a` — the default —
    /// which writes one only on a fixed, whole-disk block device, as
    /// `mkfs.fat` does; an image file gets none unless `--mbr=y`.
    #[arg(
        long,
        value_name = "y|n|a",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "y"
    )]
    mbr: Option<String>,

    /// Create DEVICE as a new file of BLOCKS × 1024 bytes, then format it.
    /// BLOCKS is required, and an existing file is never touched.
    #[arg(short = 'C')]
    create: bool,

    /// Toggle the Atari variant of the filesystem: GEMDOS's boot sector and
    /// geometry. Each `-A` flips it, as in `mkfs.fat`.
    // Append with no value, not Count: Count keeps only the last
    // occurrence's index, and the order of every `-A` is what matters.
    #[arg(
        short = 'A',
        action = clap::ArgAction::Append,
        num_args = 0,
        default_missing_value = "toggle"
    )]
    atari: Vec<String>,

    /// The variant of filesystem: `standard` or `atari`. With `-A` the last
    /// one on the command line wins, as in `mkfs.fat`.
    #[arg(long, value_name = "TYPE")]
    variant: Option<String>,

    /// Format a whole disk even though it has partitions on it, or a device
    /// that other devices are mapped onto. Without it, `mkfs.fat`'s safety
    /// check refuses.
    #[arg(short = 'I')]
    ignore_safety_checks: bool,

    /// Write the filesystem SECTOR sectors into the device — sectors of the
    /// `-S` size, or the device's (512 on an image file) — filling it from
    /// there. On a block device whose start is known (a partition, a whole
    /// disk other than a loop device) the hidden sectors become start +
    /// SECTOR unless `-H` is given, as in `mkfs.fat`; on an image file they
    /// stay 0.
    #[arg(long, value_name = "SECTOR")]
    offset: Option<String>,

    /// Use constant values wherever a timestamp or serial number would go, so
    /// two runs over the same geometry produce identical images.
    #[arg(long)]
    invariant: bool,

    /// Report what would be written and write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Say more.
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Say less.
    #[arg(short = 'q', long)]
    quiet: bool,
}

fn parse_hex(name: &str, s: &str) -> anyhow::Result<u32> {
    let cleaned = s.trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(cleaned, 16)
        .map_err(|e| anyhow::anyhow!("invalid {name} '{s}': {e}"))
}

/// A one-byte field (`-M`, `-D`): a value past 0xff is refused, never cut down
/// to its low byte.
fn parse_hex_u8(name: &str, s: &str) -> anyhow::Result<u8> {
    let v = parse_hex(name, s)?;
    u8::try_from(v).map_err(|_| anyhow::anyhow!("invalid {name} '{s}': {v:#x} is more than 0xff"))
}

/// `--offset`, read as dosfstools 4.2 reads it: `strtoll(optarg, &tmp, 0)` —
/// decimal, `0x` hex or `0` octal, an optional sign — with nothing before or
/// after the number; then a negative one refused.
fn parse_offset(s: &str) -> anyhow::Result<u64> {
    let bad = || anyhow::anyhow!("Bad number for offset : {s}");
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (radix, digits) = if let Some(h) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        (16, h)
    } else if rest.len() > 1 && rest.starts_with('0') {
        (8, &rest[1..])
    } else {
        (10, rest)
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return Err(bad());
    }
    // Past the range of a long long is strtoll's ERANGE: the same message.
    let v = u64::from_str_radix(digits, radix).map_err(|_| bad())?;
    let limit = if neg { 1u64 << 63 } else { i64::MAX as u64 };
    if v > limit {
        return Err(bad());
    }
    if neg && v != 0 {
        anyhow::bail!("FAT offset must be between 0 and {}: {s}", i64::MAX);
    }
    Ok(v)
}

/// `info->geom_start` from dosfstools 4.2 `blkdev_get_start()`: the device's
/// sysfs `start` (a partition's first sector, in 512-byte units); failing
/// that, `HDIO_GETGEO`'s start, which this crate cannot ask for without
/// `unsafe`. A whole disk's driver answers it with 0 — sd, nvme, virtio,
/// mmc, dm and md do — except the loop driver, which has no `getgeo` and
/// so no start, and that is taken here from sysfs's `loop` directory.
/// `None` (4.2's `-1`) leaves the hidden sectors alone.
fn device_start(sys: &std::path::Path) -> Option<u64> {
    if let Ok(text) = std::fs::read_to_string(sys.join("start")) {
        return text.trim().parse().ok();
    }
    if sys.join("loop").exists() {
        None
    } else {
        Some(0)
    }
}

/// The hidden sectors, as `establish_params()` sets them: `-H` if given;
/// else device start + offset when the start is known and the sum fits 32
/// bits; else 0.
fn hidden_sectors(given: Option<u32>, start: Option<u64>, offset: u64) -> Option<u32> {
    if given.is_some() {
        return given;
    }
    let sum = start?.checked_add(offset)?;
    u32::try_from(sum).ok()
}

/// What `--mbr` asked for: `Some(true)`/`Some(false)`, or `None` for auto.
/// The spellings `mkfs.fat` takes, case-insensitively.
fn parse_mbr(arg: Option<&str>) -> anyhow::Result<Option<bool>> {
    let Some(a) = arg else { return Ok(None) };
    match a.to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(Some(true)),
        "n" | "no" => Ok(Some(false)),
        "a" | "auto" => Ok(None),
        _ => anyhow::bail!("Unknown option for --mbr: '{a}' (want y, n or a)"),
    }
}

/// The variant `-A` and `--variant` leave, read in command-line order:
/// `mkfs.fat`'s getopt loop flips `atari_format` at each `-A` and sets it at
/// each `--variant`, so whichever comes last decides.
fn resolve_variant(matches: &ArgMatches) -> anyhow::Result<Variant> {
    let mut events: Vec<(usize, Option<Variant>)> = Vec::new();
    // One index per `-A`: each occurrence appends one value.
    if let Some(idx) = matches.indices_of("atari") {
        events.extend(idx.map(|i| (i, None)));
    }
    if let (Some(idx), Some(vals)) = (
        matches.indices_of("variant"),
        matches.get_many::<String>("variant"),
    ) {
        for (i, v) in idx.zip(vals) {
            let v = v.parse::<Variant>().map_err(|e| anyhow::anyhow!(e))?;
            events.push((i, Some(v)));
        }
    }
    events.sort_by_key(|&(i, _)| i);
    let mut variant = Variant::Standard;
    for (_, e) in events {
        variant = match e {
            Some(v) => v,
            None if variant == Variant::Atari => Variant::Standard,
            None => Variant::Atari,
        };
    }
    Ok(variant)
}

/// The sysfs directory of a block device, `/sys/dev/block/MAJOR:MINOR`, or
/// `None` for anything that is not a block device (an image file).
fn block_sysfs_dir(path: &str) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let meta = std::fs::metadata(path).ok()?;
        if !meta.file_type().is_block_device() {
            return None;
        }
        // glibc's major()/minor().
        let rdev = meta.rdev();
        let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & 0xffff_f000);
        let minor = (rdev & 0xff) | ((rdev >> 12) & 0xffff_ff00);
        let sys = std::path::PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
        sys.is_dir().then_some(sys)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        None
    }
}

/// `has_children` from dosfstools 4.2 `get_block_linux_info()`: `Some(true)`
/// when a subdirectory of the device's sysfs directory is a partition
/// (a regular `partition` file) that does not start at sector 0 — one that
/// does is the whole device again — or when `holders` lists a device mapped
/// onto this one; `None` when the directory could not be read, which, as
/// there, does not stop the format.
fn has_children(sys: &std::path::Path) -> Option<bool> {
    let dir = std::fs::read_dir(sys).ok()?;
    let mut unknown = false;
    for entry in dir {
        let Ok(entry) = entry else {
            unknown = true;
            continue;
        };
        // DT_DIR (or DT_UNKNOWN); the `device` and `subsystem` symlinks are
        // not followed, as readdir's d_type does not follow them.
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(true) {
            continue;
        }
        match std::fs::metadata(entry.path().join("partition")) {
            Ok(m) if m.is_file() => {
                let start = std::fs::read_to_string(entry.path().join("start"))
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .unwrap_or(-1);
                if start != 0 {
                    return Some(true);
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => unknown = true,
        }
    }
    let held = std::fs::read_dir(sys.join("holders"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    if held {
        return Some(true);
    }
    if unknown {
        None
    } else {
        Some(false)
    }
}

/// `--mbr=auto`: dosfstools 4.2 writes the table when `get_device_info()`
/// says `TYPE_FIXED` and partition 0 — a block device that is a whole disk
/// (no `partition` in sysfs), not a loop device, not a device-mapper or other
/// virtual device stacked on others (`slaves` empty), and not `removable`.
/// An image file, a partition, and anything off Linux, where it learns
/// nothing from sysfs, get none.
fn auto_mbr(path: &str) -> bool {
    let Some(sys) = block_sysfs_dir(path) else {
        return false;
    };
    if sys.join("partition").exists() || sys.join("loop").exists() {
        return false;
    }
    let stacked = std::fs::read_dir(sys.join("slaves"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    let removable = std::fs::read_to_string(sys.join("removable"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0);
    !stacked && removable == 0
}

/// `-C`: create `path` as a new file of `blocks` 1024-byte blocks, as
/// dosfstools 4.2 does: `open(O_EXCL | O_RDWR | O_CREAT, 0666)`, then
/// `ftruncate`. An existing file is refused, never truncated. Like
/// `mkfs.fat`, a file created here stays if the format then fails.
/// With `--offset` the file is that many bytes longer, `ftruncate(dev,
/// part_sector * sector_size + blocks * BLOCK_SIZE)`.
fn create_image(path: &str, blocks: Option<u64>, offset_bytes: u64) -> anyhow::Result<()> {
    let Some(blocks) = blocks else {
        anyhow::bail!("Need intended size with -C.");
    };
    let size = blocks
        .checked_mul(1024)
        .and_then(|b| b.checked_add(offset_bytes))
        .ok_or_else(|| anyhow::anyhow!("{blocks} blocks is more bytes than a u64 holds"))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o666);
    }
    let file = opts.open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => anyhow::anyhow!("file {path} already exists"),
        _ => anyhow::anyhow!("unable to create {path}: {e}"),
    })?;
    file.set_len(size)
        .map_err(|e| anyhow::anyhow!("unable to resize {path}: {e}"))?;
    Ok(())
}

/// The size to format: `blocks` 1024-byte blocks, or the whole device. A
/// count whose byte size overflows `u64` or exceeds the device is refused
/// before anything is written (finding 622740cebc3e: it used to wrap in a
/// release build, so 2^54 + 65536 blocks formatted 64 MiB).
fn format_size(blocks: Option<u64>, device: &str, device_size: u64) -> anyhow::Result<u64> {
    let Some(b) = blocks else {
        return Ok(device_size);
    };
    let size = b.checked_mul(1024).ok_or_else(|| {
        anyhow::anyhow!("{b} blocks is more bytes than a u64 holds; {device} holds {device_size}")
    })?;
    if size > device_size {
        anyhow::bail!("{b} blocks is {size} bytes; {device} holds {device_size}");
    }
    Ok(size)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let matches = Args::command().get_matches();
    let args = Args::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    let mut params = Params::new();
    if let Some(t) = &args.fat_type {
        params.fat_type = Some(t.parse::<FatType>().map_err(|e| anyhow::anyhow!(e))?);
    }
    params.sector_size = args.sector_size;
    params.sectors_per_cluster = args.sectors_per_cluster;
    params.reserved_sectors = args.reserved;
    params.num_fats = Some(args.num_fats);
    params.root_entries = args.root_entries;
    params.label = args.label.clone();
    params.hidden_sectors = args.hidden;
    params.backup_boot = args.backup_boot;
    params.align = !args.no_align;
    params.invariant = args.invariant;
    params.variant = resolve_variant(&matches)?;
    if args.fixed {
        params.disk_type = mkfs_dos::params::DiskType::Fixed;
    }
    params.mbr = match parse_mbr(args.mbr.as_deref())? {
        Some(yes) => yes,
        None => auto_mbr(&args.device),
    };
    if let Some(id) = &args.volume_id {
        params.volume_id = Some(parse_hex("volume id", id)?);
    }
    if let Some(m) = &args.media {
        params.media = Some(parse_hex_u8("media byte", m)?);
    }
    if let Some(d) = &args.drive_number {
        params.drive_number = Some(parse_hex_u8("drive number", d)?);
    }
    if let Some(g) = &args.geometry {
        let (h, s) = g
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("geometry wants heads/sectors, e.g. 255/63"))?;
        params.geometry = Some((h.parse()?, s.parse()?));
    }

    // Unlike `mkfs.fat`, which reports a file it cannot open and goes on with
    // the default message, a message that cannot be read stops the format.
    let mut message = None;
    if let Some(path) = &args.message_file {
        let raw = if path == "-" {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .map_err(|e| anyhow::anyhow!("cannot read the message from standard input: {e}"))?;
            buf
        } else {
            std::fs::read(path).map_err(|e| anyhow::anyhow!("cannot read {path}: {e}"))?
        };
        message = Some(BootMessage::encode(&raw));
        params.boot_message = Some(raw);
    }

    // Auto and specified bad block handling are mutually exclusive, and
    // `mkfs.fat` says so before it opens anything.
    if args.check && args.bad_blocks_file.is_some() {
        anyhow::bail!("-c and -l are incompatible");
    }

    if let Some(o) = &args.offset {
        params.offset = parse_offset(o)?;
    }

    if args.create {
        // An image file has no sector size of its own: `-S`, or 512.
        let ss = params.sector_size.unwrap_or(512) as u64;
        let offset_bytes = params
            .offset
            .checked_mul(ss)
            .ok_or_else(|| anyhow::anyhow!("an offset of {} sectors overflows", params.offset))?;
        create_image(&args.device, args.blocks, offset_bytes)?;
    }

    let device = FileDevice::open(&args.device)
        .await
        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", args.device))?;

    let sysfs = block_sysfs_dir(&args.device);
    // `mkfs.fat` refuses a whole disk with partitions on it, or a device
    // something is mapped onto, unless `-I`.
    if !args.ignore_safety_checks && sysfs.as_deref().and_then(has_children) == Some(true) {
        anyhow::bail!(
            "Partitions or virtual mappings on device '{}', not making filesystem (use -I to override)",
            args.device
        );
    }
    // A block device's sector size counts as given (`sector_size_set`), so
    // the Atari variant refuses rather than enlarging its logical sectors.
    if sysfs.is_some() && params.sector_size.is_none() {
        params.sector_size = Some(device.logical_sector_size());
    }
    // Hidden sectors: the device's start plus the offset, where 4.2 knows the
    // start — never on an image file.
    params.hidden_sectors = hidden_sectors(
        params.hidden_sectors,
        sysfs.as_deref().and_then(device_start),
        params.offset,
    );

    // `--offset`, in sectors of the size asked for or the device's (512 on an
    // image file), before the Atari search can enlarge it.
    let sector0 = params
        .sector_size
        .unwrap_or(device.logical_sector_size())
        .max(device.logical_sector_size()) as u64;
    let offset_bytes = params
        .offset
        .checked_mul(sector0)
        .ok_or_else(|| anyhow::anyhow!("an offset of {} sectors overflows", params.offset))?;
    if params.offset > 0 && device.size() <= offset_bytes {
        anyhow::bail!(
            "The device {} size {} is less than the offset {offset_bytes}",
            args.device,
            device.size()
        );
    }

    // A block count names a filesystem of that size inside the device, which is
    // how `mkfs.fat` takes it — 1024-byte blocks, whatever the sector size —
    // from the offset on. The defaults that come from the device itself are
    // taken from all of it, as `establish_params()` takes them.
    let room = device.size() - offset_bytes;
    if let Some(b) = args.blocks {
        if b != room / 1024 {
            eprintln!("Warning: block count mismatch: found {} but assuming {b}.", room / 1024);
        }
    }
    let size = format_size(args.blocks, &args.device, room)?;
    let fs_bytes = args.blocks.map(|_| size);

    let geometry =
        Geometry::compute_sized(device.size(), fs_bytes, device.logical_sector_size(), &params)?;

    // The warnings `mkfs.fat` gives, in its order: the message overran the
    // boot code area; a sector size past 4096; then FAT32's shorter area.
    if let Some(message) = &message {
        if message.truncated {
            eprintln!("Warning: message too long; truncated");
        }
    }
    // mkfs.fat formats sectors up to 32768 bytes and says so above 4096 —
    // of the size asked for (or the device's), before the Atari search grows
    // its logical sectors, which it does without a warning.
    let asked = params
        .sector_size
        .unwrap_or(device.logical_sector_size())
        .max(device.logical_sector_size());
    if asked > 4096 {
        eprintln!(
            "Warning: sector size {asked} > 4096 is non-standard, filesystem may not be usable"
        );
    }
    if let Some(message) = &message {
        if geometry.fat_type == FatType::Fat32 && !message.fits_fat32() {
            eprintln!("Warning: message too long; truncated");
        }
    }

    if geometry.variant == Variant::Atari && geometry.total_sectors >= OLDGEMDOS_MAX_SECTORS {
        println!("Warning: More than 32765 sector need TOS 1.04 or higher.");
    }

    // Bad blocks, where `mkfs.fat` finds them: after `setup_tables()`,
    // before anything is written. Every block is checked here, so a refused
    // one stops the format with the device untouched (and --dry-run says so).
    if let Some(path) = &args.bad_blocks_file {
        let text = std::fs::read(path)
            .map_err(|e| anyhow::anyhow!("Can't open file of bad blocks {path}: {e}"))?;
        params.bad_blocks = mkfs_dos::badblocks::parse_list(&text)?;
    } else if args.check {
        if args.verbose {
            print!("Searching for bad blocks ");
            std::io::stdout().flush().ok();
        }
        // From the offset on: 4.2's `do_check()` seeks there too, then
        // compares the position with one that leaves the offset out, so
        // `-c --offset` with an offset always dies there; here it checks.
        let window = OffsetDevice::new(&device, geometry.offset)?;
        let bad = mkfs_dos::badblocks::check_blocks(&window, size / 1024).await;
        if args.verbose {
            println!();
        }
        mkfs_dos::badblocks::check_before_data(&geometry, &bad)?;
        params.bad_blocks = bad;
    }
    mkfs_dos::badblocks::bad_clusters(&geometry, &params.bad_blocks)?;
    if !params.bad_blocks.is_empty() && !args.quiet {
        let n = params.bad_blocks.len();
        println!("{n} bad block{}", if n > 1 { "s" } else { "" });
    }

    if args.dry_run {
        print_geometry(&geometry);
        println!("MBR partition table:  {}", if params.mbr { "yes" } else { "no" });
        println!("\nNothing was written (--dry-run).");
        return Ok(());
    }

    // The geometry was resolved above so that --dry-run and the real run print
    // the same numbers; formatting to it writes exactly what was printed.
    let report = format_with(&device, &geometry, &params).await?;

    if report.message_cut_by_mbr {
        eprintln!("Warning: message too long; truncated");
    }
    if args.verbose {
        print_geometry(&geometry);
        if let Some(sig) = report.disk_signature {
            println!("Adding MBR table:     disk signature {sig:08x}");
        }
    }
    if !args.quiet {
        println!("{}: {report}", args.device);
    }
    Ok(())
}

fn print_geometry(g: &Geometry) {
    println!("Filesystem type:      {}", g.fat_type);
    println!("Sector size:          {}", g.sector_size);
    println!("Cluster size:         {} ({} sectors)", g.cluster_size(), g.sectors_per_cluster);
    println!("Total sectors:        {}", g.total_sectors);
    if g.offset > 0 {
        println!("Offset:               {} bytes into the device", g.offset);
    }
    println!("Hidden sectors:       {}", g.hidden_sectors);
    println!("Reserved sectors:     {}", g.reserved_sectors);
    println!("FATs:                 {} of {} sectors", g.num_fats, g.fat_length);
    if g.fat_type == FatType::Fat32 {
        println!("Root cluster:         {}", g.root_cluster);
        println!("FSInfo sector:        {}", g.info_sector);
        println!("Backup boot sector:   {}", g.backup_boot);
    } else {
        println!("Root entries:         {} in {} sectors", g.root_entries, g.root_dir_sectors);
    }
    println!("Clusters:             {}", g.cluster_count);
    println!("Data area starts at:  sector {}", g.first_data_sector());
    println!("Media descriptor:     {:#04x}", g.media);
    println!("Geometry:             {} heads, {} sectors per track", g.heads, g.sectors_per_track);
    println!("Aligned:              {}", if g.aligned { "yes" } else { "no" });
    if g.variant == Variant::Atari {
        println!("Variant:              Atari");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Finding 8a2452f8f3a5: -M 0x1f8 and -D 0x180 are refused, not written
    // as 0xf8 and 0x80.
    #[test]
    fn a_byte_option_past_0xff_is_refused() {
        assert_eq!(parse_hex_u8("media byte", "0xf8").unwrap(), 0xf8);
        assert_eq!(parse_hex_u8("drive number", "80").unwrap(), 0x80);
        assert_eq!(parse_hex_u8("media byte", "0xff").unwrap(), 0xff);
        assert!(parse_hex_u8("media byte", "0x1f8").is_err());
        assert!(parse_hex_u8("drive number", "0x180").is_err());
    }

    // --mbr (#8): mkfs.fat's spellings; a bare --mbr is y; absent is auto.
    #[test]
    fn mbr_takes_mkfs_fat_spellings() {
        assert_eq!(parse_mbr(Some("y")).unwrap(), Some(true));
        assert_eq!(parse_mbr(Some("YES")).unwrap(), Some(true));
        assert_eq!(parse_mbr(Some("n")).unwrap(), Some(false));
        assert_eq!(parse_mbr(Some("No")).unwrap(), Some(false));
        assert_eq!(parse_mbr(Some("a")).unwrap(), None);
        assert_eq!(parse_mbr(Some("auto")).unwrap(), None);
        assert_eq!(parse_mbr(None).unwrap(), None);
        assert!(parse_mbr(Some("maybe")).is_err());

        let bare = Args::try_parse_from(["mkfs.fat", "--mbr", "img"]).unwrap();
        assert_eq!(bare.mbr.as_deref(), Some("y"));
        assert_eq!(bare.device, "img");
        let off = Args::try_parse_from(["mkfs.fat", "--mbr=n", "img"]).unwrap();
        assert_eq!(off.mbr.as_deref(), Some("n"));
        let none = Args::try_parse_from(["mkfs.fat", "img"]).unwrap();
        assert_eq!(none.mbr, None);
    }

    // -A toggles, --variant sets, and the last one on the line wins (#11).
    #[test]
    fn the_variant_follows_command_line_order() {
        let v = |argv: &[&str]| {
            let mut full = vec!["mkfs.fat"];
            full.extend_from_slice(argv);
            full.push("img");
            let m = Args::command().try_get_matches_from(full).unwrap();
            resolve_variant(&m)
        };
        assert_eq!(v(&[]).unwrap(), Variant::Standard);
        assert_eq!(v(&["-A"]).unwrap(), Variant::Atari);
        assert_eq!(v(&["-A", "-A"]).unwrap(), Variant::Standard);
        assert_eq!(v(&["-A", "-A", "-A"]).unwrap(), Variant::Atari);
        assert_eq!(v(&["--variant", "atari"]).unwrap(), Variant::Atari);
        assert_eq!(v(&["--variant=ATARI", "-A"]).unwrap(), Variant::Standard);
        assert_eq!(v(&["-A", "--variant", "standard"]).unwrap(), Variant::Standard);
        assert_eq!(v(&["-A", "--variant", "atari"]).unwrap(), Variant::Atari);
        assert_eq!(v(&["--variant", "standard", "-A"]).unwrap(), Variant::Atari);
        assert!(v(&["--variant", "amiga"]).is_err());
        let a = Args::try_parse_from(["mkfs.fat", "-I", "img"]).unwrap();
        assert!(a.ignore_safety_checks);
        assert!(!Args::try_parse_from(["mkfs.fat", "img"]).unwrap().ignore_safety_checks);
    }

    // -I (#11): has_children as dosfstools reads sysfs, over a fake tree.
    #[test]
    fn a_disk_with_partitions_or_holders_has_children() {
        let root = std::env::temp_dir().join(format!("mkfs-fat-sysfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let disk = root.join("8:0");
        std::fs::create_dir_all(disk.join("holders")).unwrap();
        std::fs::create_dir_all(disk.join("queue")).unwrap();
        std::fs::write(disk.join("size"), "131072\n").unwrap();
        assert_eq!(has_children(&disk), Some(false));

        // A partition that starts at 0 is the whole disk again, not a child.
        std::fs::create_dir_all(disk.join("sda1")).unwrap();
        std::fs::write(disk.join("sda1/partition"), "1\n").unwrap();
        std::fs::write(disk.join("sda1/start"), "0\n").unwrap();
        assert_eq!(has_children(&disk), Some(false));
        std::fs::write(disk.join("sda1/start"), "2048\n").unwrap();
        assert_eq!(has_children(&disk), Some(true));
        std::fs::remove_dir_all(disk.join("sda1")).unwrap();

        // A holder (device-mapper, md) is a child too.
        std::fs::write(disk.join("holders/dm-0"), "").unwrap();
        assert_eq!(has_children(&disk), Some(true));

        assert_eq!(has_children(&root.join("missing")), None);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(block_sysfs_dir("/nonexistent/device").is_none());
    }

    // -C (#6): BLOCKS required, the file made at BLOCKS x 1024 bytes, and an
    // existing file refused and left as it was.
    #[test]
    fn create_makes_a_new_file_and_never_an_existing_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.img");
        let p = path.to_str().unwrap();
        let err = create_image(p, None, 0).unwrap_err();
        assert_eq!(err.to_string(), "Need intended size with -C.");
        assert!(!path.exists());

        create_image(p, Some(1440), 0).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 1440 * 1024);

        std::fs::write(&path, b"keep").unwrap();
        let err = create_image(p, Some(1440), 0).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");

        assert!(create_image(dir.path().join("x").to_str().unwrap(), Some(u64::MAX), 0).is_err());
        // --offset: the file is the offset longer.
        let off = dir.path().join("off.img");
        create_image(off.to_str().unwrap(), Some(16384), 2048 * 512).unwrap();
        assert_eq!(std::fs::metadata(&off).unwrap().len(), 17 << 20);
        assert!(create_image(dir.path().join("y").to_str().unwrap(), Some(1 << 53), u64::MAX).is_err());
        let a = Args::try_parse_from(["mkfs.fat", "-C", "img", "1440"]).unwrap();
        assert!(a.create);
        assert_eq!(a.blocks, Some(1440));
    }

    // --offset (#9): strtoll(…, 0) and 4.2's two refusals.
    #[test]
    fn offset_is_read_as_mkfs_fat_reads_it() {
        assert_eq!(parse_offset("2048").unwrap(), 2048);
        assert_eq!(parse_offset("0x800").unwrap(), 2048);
        assert_eq!(parse_offset("0X800").unwrap(), 2048);
        assert_eq!(parse_offset("04000").unwrap(), 2048);
        assert_eq!(parse_offset("+63").unwrap(), 63);
        assert_eq!(parse_offset("0").unwrap(), 0);
        assert_eq!(parse_offset("-0").unwrap(), 0);
        assert_eq!(parse_offset("9223372036854775807").unwrap(), i64::MAX as u64);
        for bad in ["", " 8", "8 ", "8x", "08", "0x", "-", "+", "9223372036854775808", "1e3"] {
            let err = parse_offset(bad).unwrap_err().to_string();
            assert!(err.starts_with("Bad number for offset"), "{bad:?}: {err}");
        }
        for neg in ["-1", "-0x10", "-9223372036854775808"] {
            let err = parse_offset(neg).unwrap_err().to_string();
            assert!(err.starts_with("FAT offset must be between 0 and"), "{neg:?}: {err}");
        }
        let a = Args::try_parse_from(["mkfs.fat", "--offset", "2048", "img"]).unwrap();
        assert_eq!(a.offset.as_deref(), Some("2048"));
    }

    // Hidden sectors: -H wins; else the device's start plus the offset where
    // 4.2 knows the start (a partition's sysfs `start`, a whole disk's 0, a
    // loop device's none); never past 32 bits.
    #[test]
    fn hidden_sectors_follow_the_device_start() {
        assert_eq!(hidden_sectors(Some(7), Some(2048), 63), Some(7));
        assert_eq!(hidden_sectors(None, None, 2048), None);
        assert_eq!(hidden_sectors(None, Some(0), 2048), Some(2048));
        assert_eq!(hidden_sectors(None, Some(2048), 0), Some(2048));
        assert_eq!(hidden_sectors(None, Some(2048), 63), Some(2111));
        assert_eq!(hidden_sectors(None, Some(u32::MAX as u64), 1), None);

        let root = std::env::temp_dir().join(format!("mkfs-fat-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let part = root.join("8:1");
        std::fs::create_dir_all(&part).unwrap();
        std::fs::write(part.join("start"), "2048\n").unwrap();
        assert_eq!(device_start(&part), Some(2048));
        let disk = root.join("8:0");
        std::fs::create_dir_all(&disk).unwrap();
        assert_eq!(device_start(&disk), Some(0));
        let lo = root.join("7:0");
        std::fs::create_dir_all(lo.join("loop")).unwrap();
        assert_eq!(device_start(&lo), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    // Auto never writes a table on an image file or a non-block device.
    #[test]
    fn auto_mbr_is_no_for_a_file() {
        let path = std::env::temp_dir().join(format!("mkfs-fat-auto-mbr-{}", std::process::id()));
        std::fs::write(&path, [0u8; 512]).unwrap();
        assert!(!auto_mbr(path.to_str().unwrap()));
        std::fs::remove_file(&path).unwrap();
        assert!(!auto_mbr("/dev/null"));
        assert!(!auto_mbr("/nonexistent/device"));
    }

    // Finding 622740cebc3e: a block count whose byte size overflows is
    // refused, not wrapped into a smaller size that fits the device.
    #[test]
    fn an_overflowing_block_count_is_refused() {
        let dev = 64 << 20;
        assert_eq!(format_size(None, "img", dev).unwrap(), dev);
        assert_eq!(format_size(Some(65536), "img", dev).unwrap(), dev);
        assert!(format_size(Some(65537), "img", dev).is_err());
        // (2^54 + 65536) * 1024 wraps to exactly 64 MiB.
        assert_eq!(((1u64 << 54) + 65536).wrapping_mul(1024), dev);
        assert!(format_size(Some((1 << 54) + 65536), "img", dev).is_err());
        assert!(format_size(Some(u64::MAX), "img", dev).is_err());
    }
}
