//! Build a filesystem image, for the verification harness and for looking at
//! one by hand.
//!
//! ```sh
//! cargo run --example mkimage -- out.img 64 fat16 label=ESP
//! ```
//!
//! Arguments after the size are `name=value` pairs, or a bare FAT width. The
//! point of the example is that it uses nothing the library does not export:
//! a device, some parameters, and one call.

use mkfs_dos::{format, FatType, FileDevice, Params};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: mkimage <path> <size> [options]")?;
    // Sizes are MiB unless they carry a `k` — a 1.44 MB floppy is 1440 KiB and
    // not any whole number of mebibytes, and it is the one size whose defaults
    // differ.
    let size_arg = args.next().ok_or("missing size (MiB, or KiB with a 'k' suffix)")?;
    let size = match size_arg.strip_suffix('k').or_else(|| size_arg.strip_suffix('K')) {
        Some(kib) => kib.parse::<u64>()? * 1024,
        None => size_arg.parse::<u64>()? * 1024 * 1024,
    };

    let mut params = Params::new().invariant();
    for arg in args {
        match arg.split_once('=') {
            Some(("label", v)) => params.label = Some(v.to_string()),
            Some(("sector", v)) => params.sector_size = Some(v.parse()?),
            Some(("cluster", v)) => params.sectors_per_cluster = Some(v.parse()?),
            Some(("fats", v)) => params.num_fats = Some(v.parse()?),
            Some(("root", v)) => params.root_entries = Some(v.parse()?),
            Some(("reserved", v)) => params.reserved_sectors = Some(v.parse()?),
            Some((k, _)) => return Err(format!("unknown option '{k}'").into()),
            None => match arg.as_str() {
                "noalign" => params.align = false,
                "fixed" => params.disk_type = mkfs_dos::DiskType::Fixed,
                width => params.fat_type = Some(width.parse::<FatType>()?),
            },
        }
    }

    let device = FileDevice::create(&path, size).await?;
    let report = format(&device, &params).await?;
    println!("{path}: {report}");
    Ok(())
}
