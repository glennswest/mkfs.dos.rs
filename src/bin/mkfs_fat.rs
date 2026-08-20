//! `mkfs.fat` — create a FAT12, FAT16 or FAT32 filesystem.
//!
//! Flags follow dosfstools' `mkfs.fat` where they mean the same thing, so
//! muscle memory and existing scripts carry over.

use clap::Parser;

use mkfs_dos::device::{BlockDevice, FileDevice};
use mkfs_dos::format::format_with;
use mkfs_dos::layout::Geometry;
use mkfs_dos::params::{FatType, Params};

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

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
    if args.fixed {
        params.disk_type = mkfs_dos::params::DiskType::Fixed;
    }
    if let Some(id) = &args.volume_id {
        params.volume_id = Some(parse_hex("volume id", id)?);
    }
    if let Some(m) = &args.media {
        params.media = Some(parse_hex("media byte", m)? as u8);
    }
    if let Some(d) = &args.drive_number {
        params.drive_number = Some(parse_hex("drive number", d)? as u8);
    }
    if let Some(g) = &args.geometry {
        let (h, s) = g
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("geometry wants heads/sectors, e.g. 255/63"))?;
        params.geometry = Some((h.parse()?, s.parse()?));
    }

    let device = FileDevice::open(&args.device)
        .await
        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", args.device))?;

    // A block count names a filesystem of that size inside the device, which is
    // how `mkfs.fat` takes it — 1024-byte blocks, whatever the sector size.
    let size = args
        .blocks
        .map(|b| b * 1024)
        .unwrap_or_else(|| device.size());
    if size > device.size() {
        anyhow::bail!(
            "{} blocks is {size} bytes; {} holds {}",
            args.blocks.unwrap_or(0),
            args.device,
            device.size()
        );
    }

    let geometry = Geometry::compute(size, device.logical_sector_size(), &params)?;

    if args.dry_run {
        print_geometry(&geometry);
        println!("\nNothing was written (--dry-run).");
        return Ok(());
    }

    // The geometry was resolved above so that --dry-run and the real run print
    // the same numbers; formatting to it writes exactly what was printed.
    let report = format_with(&device, &geometry, &params).await?;

    if args.verbose {
        print_geometry(&geometry);
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
}
