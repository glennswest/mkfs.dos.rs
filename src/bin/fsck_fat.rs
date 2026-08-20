//! `fsck.fat` — check, and optionally repair, a FAT filesystem.
//!
//! Exit codes follow `fsck.fat`: 0 clean, 1 errors corrected, 4 errors left
//! uncorrected, 8 an operational error.

use clap::Parser;

use mkfs_dos::device::FileDevice;
use mkfs_dos::fsck::{check, FsckOptions, Severity};

#[derive(Parser, Debug)]
#[command(name = "fsck.fat", about = "Check a FAT12/FAT16/FAT32 filesystem", version)]
struct Args {
    /// Device or image file to check.
    device: String,

    /// Repair what can be repaired. Without it, nothing is written.
    #[arg(short = 'a', long)]
    repair: bool,

    /// Check only; never write. The default, and the opposite of -a.
    #[arg(short = 'n', long, conflicts_with = "repair")]
    check_only: bool,

    /// Report findings that are unusual but not wrong.
    #[arg(short = 'v', long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = Args::parse();
    match run(&args).await {
        Ok(code) => std::process::ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("fsck.fat: {e}");
            // 8 is `fsck.fat`'s "operational error" — the filesystem was not
            // checked at all, which is a different thing from finding it dirty.
            std::process::ExitCode::from(8)
        }
    }
}

async fn run(args: &Args) -> anyhow::Result<i32> {
    let options = FsckOptions {
        repair: args.repair,
        verbose: args.verbose,
    };

    let device = FileDevice::open(&args.device)
        .await
        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", args.device))?;
    let report = check(&device, &options).await?;

    for problem in &report.problems {
        let mark = match (problem.severity, problem.fixed) {
            (Severity::Info, _) => "note",
            (_, true) => "fixed",
            (Severity::Fixable, false) => "fixable",
            (Severity::Serious, false) => "ERROR",
        };
        println!("pass {} [{mark}] {}", problem.pass, problem.message);
    }

    println!(
        "{}: {} files, {} directories, {}/{} clusters used",
        args.device, report.files, report.directories, report.clusters_used, report.cluster_count
    );
    if report.is_clean() {
        println!("{}: clean", args.device);
    } else if report.unfixed().next().is_some() && !args.repair {
        println!("{}: run with -a to repair what can be repaired", args.device);
    }

    Ok(report.exit_code())
}
