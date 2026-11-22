//! Bad blocks: `-l FILE` and `-c`, after `get_list_blocks()`,
//! `check_blocks()` and `mark_FAT_sector()` in dosfstools 4.2 `mkfs.fat.c`.
//!
//! A bad block is a 1024-byte block (`BLOCK_SIZE`), numbered from the start
//! of the filesystem whatever the sector size. `mkfs.fat` turns each into its
//! two 512-byte "hard" sectors and marks the cluster holding each one bad —
//! `0xff7`, `0xfff7` or `0x0ffffff7` — in every FAT. Nothing else changes:
//! not the geometry, not the data area, and not the FAT32 FSInfo free count,
//! which 4.2 leaves at `cluster_count - 1` (so `fsck.fat` reports the free
//! count of such a volume as wrong; it is a hint, and this crate writes what
//! 4.2 writes).
//!
//! The checks are 4.2's, in its order, for each of a block's two sectors:
//!
//! 1. a sector before the data area is refused — `Block number N is before
//!    data area`;
//! 2. a sector at or past `num_sectors` is refused — `Block number N is
//!    behind end of filesystem`. 4.2 compares a 512-byte sector number with a
//!    count of *logical* sectors here, so on 4096-byte sectors only blocks in
//!    the first eighth of the volume pass. That limit is kept: a block 4.2
//!    refuses is refused here too;
//! 3. a sector past the last cluster (the tail the cluster count does not
//!    cover) makes 4.2 die with `Internal error: out of range cluster number`;
//!    it is refused here with a message saying which block.
//!
//! One check is this crate's own: on FAT32, a block in the root directory's
//! cluster is refused. 4.2 marks that cluster bad over the root's
//! end-of-chain and writes a volume whose root directory is a bad cluster.

use futures::future::join_all;

use crate::device::BlockDevice;
use crate::error::{Error, Result};
use crate::layout::Geometry;
use crate::params::FatType;

/// `BLOCK_SIZE`: the unit bad blocks are counted in.
pub const BLOCK_SIZE: u64 = 1024;
/// `HARD_SECTOR_SIZE`.
const HARD_SECTOR_SIZE: u64 = 512;
/// `SECTORS_PER_BLOCK`.
const SECTORS_PER_BLOCK: u64 = BLOCK_SIZE / HARD_SECTOR_SIZE;
/// `TEST_BUFFER_BLOCKS`: blocks read at a time by [`check_blocks`].
pub const TEST_BUFFER_BLOCKS: u64 = 16;

/// Parse a bad block list, as `get_list_blocks()` reads the `-l` file.
///
/// One decimal block number per line, as `strtoll(line, &end, 10)` takes it:
/// leading white space and a `+` allowed, nothing but white space after the
/// digits, and the line ends at a NUL. Empty and white-space-only lines are
/// skipped. A negative number, one past `i64::MAX`, or a badly formed line is
/// refused. Duplicates are kept: `mkfs.fat` counts each line.
pub fn parse_list(text: &[u8]) -> Result<Vec<u64>> {
    let mut blocks = Vec::new();
    for (index, raw) in text.split_inclusive(|&b| b == b'\n').enumerate() {
        let lineno = index + 1;
        // C string handling: everything after a NUL is invisible.
        let line = match raw.iter().position(|&b| b == 0) {
            Some(nul) => &raw[..nul],
            None => raw,
        };
        let mut i = 0;
        while i < line.len() && is_c_space(line[i]) {
            i += 1;
        }
        let negative = match line.get(i) {
            Some(b'-') => {
                i += 1;
                true
            }
            Some(b'+') => {
                i += 1;
                false
            }
            _ => false,
        };
        let digits_start = i;
        let mut value: u64 = 0;
        let mut overflow = false;
        while i < line.len() && line[i].is_ascii_digit() {
            value = match value
                .checked_mul(10)
                .and_then(|v| v.checked_add((line[i] - b'0') as u64))
            {
                Some(v) => v,
                None => {
                    overflow = true;
                    u64::MAX
                }
            };
            i += 1;
        }
        let converted = i > digits_start;
        // With no digits strtoll consumes nothing: `end == line`.
        let rest = if converted { &line[i..] } else { line };
        let limit = if negative { 1u64 << 63 } else { i64::MAX as u64 };
        if converted && (overflow || value > limit) {
            return Err(list_error(format!(
                "while converting bad block number in line {lineno}: Numerical result out of range"
            )));
        }
        if converted && negative && value != 0 {
            return Err(list_error(format!(
                "while converting bad block number in line {lineno}: negative block number"
            )));
        }
        if rest.iter().any(|&b| !is_c_space(b)) {
            return Err(list_error(format!(
                "badly formed number in bad blocks file line {lineno}"
            )));
        }
        if converted {
            blocks.push(value);
        }
    }
    Ok(blocks)
}

/// `isspace()` in the C locale.
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn list_error(detail: String) -> Error {
    Error::InvalidParams(format!("error in bad blocks file: {detail}"))
}

/// The clusters a list of bad blocks marks, sorted and without duplicates,
/// after the checks in the module documentation. `Err` names the first
/// block refused; nothing has been written by then.
pub fn bad_clusters(geometry: &Geometry, blocks: &[u64]) -> Result<Vec<u32>> {
    // `start_data_sector` is in hard sectors.
    let per_sector = (geometry.sector_size as u64 / HARD_SECTOR_SIZE).max(1);
    let start_data = geometry.first_data_sector() as u64 * per_sector;
    let num_sectors = geometry.total_sectors as u64;
    let hard_per_cluster = geometry.sectors_per_cluster as u64 * per_sector;
    let mut clusters = Vec::with_capacity(blocks.len());
    for &block in blocks {
        for i in 0..SECTORS_PER_BLOCK {
            let sector = (block as u128) * SECTORS_PER_BLOCK as u128 + i as u128;
            if sector < start_data as u128 {
                return Err(Error::InvalidParams(format!(
                    "Block number {block} is before data area"
                )));
            }
            if sector >= num_sectors as u128 {
                return Err(Error::InvalidParams(format!(
                    "Block number {block} is behind end of filesystem"
                )));
            }
            let sector = sector as u64;
            let cluster = (sector - start_data) / hard_per_cluster + 2;
            if cluster >= geometry.fat_entries() as u64 {
                return Err(Error::InvalidParams(format!(
                    "Block number {block} is past the last cluster ({}); mkfs.fat 4.2 stops with an internal error here",
                    geometry.fat_entries() - 1
                )));
            }
            let cluster = cluster as u32;
            if geometry.fat_type == FatType::Fat32 && cluster == geometry.root_cluster {
                return Err(Error::InvalidParams(format!(
                    "Block number {block} is in cluster {cluster}, the root directory's; the root cannot start in a bad cluster"
                )));
            }
            clusters.push(cluster);
        }
    }
    clusters.sort_unstable();
    clusters.dedup();
    Ok(clusters)
}

/// `-c`: read every 1024-byte block of the first `blocks` and return the
/// numbers of those that cannot be read, as `check_blocks()` finds them.
///
/// Blocks are read [`TEST_BUFFER_BLOCKS`] at a time; a run that fails is read
/// again block by block, and each block whose read fails is bad. (4.2 learns
/// how far a short `read()` got and marks the block after it; a
/// [`BlockDevice`] read either succeeds or fails, so the run is retried.)
/// Reads past the end of the device fail, and those blocks are bad, as they
/// are there.
pub async fn check_blocks<D: BlockDevice>(device: &D, blocks: u64) -> Vec<u64> {
    let mut bad = Vec::new();
    let mut buf = vec![0u8; (TEST_BUFFER_BLOCKS * BLOCK_SIZE) as usize];
    let mut current = 0u64;
    while current < blocks {
        let n = TEST_BUFFER_BLOCKS.min(blocks - current);
        let len = (n * BLOCK_SIZE) as usize;
        if device
            .read_at(current * BLOCK_SIZE, &mut buf[..len])
            .await
            .is_err()
        {
            let device = &device;
            let reads = (current..current + n).map(|block| async move {
                let mut one = vec![0u8; BLOCK_SIZE as usize];
                (block, device.read_at(block * BLOCK_SIZE, &mut one).await.is_err())
            });
            for (block, failed) in join_all(reads).await {
                if failed {
                    bad.push(block);
                }
            }
        }
        current += n;
    }
    bad
}

/// `-c` refuses a bad block before the data area with its own message.
pub fn check_before_data(geometry: &Geometry, bad: &[u64]) -> Result<()> {
    let per_sector = (geometry.sector_size as u64 / HARD_SECTOR_SIZE).max(1);
    let start_data_sector = geometry.first_data_sector() as u64 * per_sector;
    let start_data_block = start_data_sector.div_ceil(SECTORS_PER_BLOCK);
    if bad.iter().any(|&b| b < start_data_block) {
        return Err(Error::InvalidParams(
            "bad blocks before data-area: cannot make fs".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Params;

    fn geom(size: u64, params: &Params) -> Geometry {
        Geometry::compute(size, 512, params).unwrap()
    }

    #[test]
    fn the_list_is_read_as_strtoll_reads_it() {
        let text = b"17\n  18 \n\n\t\n+100\r\n0186\n-0\n1439";
        assert_eq!(parse_list(text).unwrap(), vec![17, 18, 100, 186, 0, 1439]);
        assert_eq!(parse_list(b"").unwrap(), Vec::<u64>::new());
        // A NUL ends the line, as it ends the C string.
        assert_eq!(parse_list(b"20\0junk\n").unwrap(), vec![20]);
        // Duplicates are kept: each line counts.
        assert_eq!(parse_list(b"20\n20\n").unwrap(), vec![20, 20]);
    }

    #[test]
    fn a_malformed_list_is_refused() {
        for bad in [
            &b"12x\n"[..],
            b"0x10\n",
            b"abc\n",
            b"1 2\n",
            b"-1\n",
            b"+\n",
            b"99999999999999999999\n",
            b"9223372036854775808\n",
        ] {
            assert!(parse_list(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        assert!(parse_list(b"9223372036854775807\n").is_ok());
        let err = parse_list(b"5\n6\nseven\n").unwrap_err().to_string();
        assert!(err.contains("line 3"), "{err}");
    }

    #[test]
    fn blocks_map_to_clusters_as_mark_fat_sector_maps_them() {
        // 1.44 MB floppy: data at hard sector 33, one-sector clusters.
        let g = geom(1440 * 1024, &Params::new());
        assert_eq!(g.first_data_sector(), 33);
        assert_eq!(g.sectors_per_cluster, 1);
        // Block 17 is sectors 34 and 35: clusters 3 and 4.
        assert_eq!(bad_clusters(&g, &[17]).unwrap(), vec![3, 4]);
        // Block 1439, the last: sectors 2878/2879, clusters 2847/2848.
        assert_eq!(bad_clusters(&g, &[1439]).unwrap(), vec![2847, 2848]);
        assert!(bad_clusters(&g, &[16]).unwrap_err().to_string().contains("before data area"));
        assert!(bad_clusters(&g, &[1440]).unwrap_err().to_string().contains("behind end"));
    }

    #[test]
    fn larger_sectors_keep_the_4_2_limit() {
        // 4096-byte sectors: 4.2 compares hard sectors with logical ones, so
        // only blocks below total_sectors / 2 are taken.
        let p = Params::new().sector_size(4096);
        let g = Geometry::compute(1024 * 1024 * 1024, 512, &p).unwrap();
        let limit = g.total_sectors as u64 / 2;
        let data_block = (g.first_data_sector() as u64 * 8).div_ceil(2);
        // A block in cluster 3, well inside the limit.
        let c3 = data_block + (g.sectors_per_cluster as u64 * 8) / 2;
        assert_eq!(bad_clusters(&g, &[c3]).unwrap(), vec![3]);
        assert!(bad_clusters(&g, &[limit]).unwrap_err().to_string().contains("behind end"));
    }

    #[test]
    fn the_fat32_root_cluster_is_refused() {
        let g = geom(512 * 1024 * 1024, &Params::new());
        assert_eq!(g.fat_type, FatType::Fat32);
        let first = g.first_data_sector() as u64 / 2;
        let err = bad_clusters(&g, &[first]).unwrap_err().to_string();
        assert!(err.contains("root directory"), "{err}");
        // The first block of cluster 3 is fine.
        let c3 = first + g.sectors_per_cluster as u64 / 2;
        assert_eq!(bad_clusters(&g, &[c3]).unwrap(), vec![3]);
    }

    #[test]
    fn a_block_past_the_last_cluster_is_refused() {
        // A geometry whose sectors run past its last cluster (the tail
        // `mkfs.fat` leaves when the data area is not a whole number of
        // clusters): the block holding the first uncovered sector.
        let mut g = geom(16 * 1024 * 1024, &Params::new());
        let covered = g.first_data_sector() as u64
            + g.cluster_count as u64 * g.sectors_per_cluster as u64;
        g.total_sectors = (covered + 8) as u32;
        let err = bad_clusters(&g, &[covered.div_ceil(2)]).unwrap_err().to_string();
        assert!(err.contains("past the last cluster"), "{err}");
        assert!(bad_clusters(&g, &[covered / 2 - 1]).is_ok());
    }

    struct Flaky {
        inner: crate::MemDevice,
        bad: Vec<u64>,
    }

    #[async_trait::async_trait]
    impl BlockDevice for Flaky {
        fn size(&self) -> u64 {
            self.inner.size()
        }
        async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
            let first = offset / BLOCK_SIZE;
            let last = (offset + buf.len() as u64 - 1) / BLOCK_SIZE;
            if self.bad.iter().any(|b| (first..=last).contains(b)) {
                return Err(Error::Io {
                    offset,
                    source: std::io::Error::other("unreadable"),
                });
            }
            self.inner.read_at(offset, buf).await
        }
        async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
            self.inner.write_at(offset, buf).await
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn check_finds_the_unreadable_blocks() {
        let dev = Flaky {
            inner: crate::MemDevice::new(1440 * 1024),
            bad: vec![17, 31, 32, 1439],
        };
        assert_eq!(check_blocks(&dev, 1440).await, vec![17, 31, 32, 1439]);
        let g = geom(1440 * 1024, &Params::new());
        assert!(check_before_data(&g, &[17]).is_ok());
        assert!(check_before_data(&g, &[16]).is_err());
    }
}
