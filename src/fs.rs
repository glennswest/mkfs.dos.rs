//! The read layer: open a volume and walk it.
//!
//! Everything above this — the checker here, the file I/O in `fio-dos` — works
//! through [`Filesystem`]. It owns the boot sector, the arithmetic that turns a
//! cluster number into a byte offset, and the FAT accessors, and it owns them
//! once so that two implementations of "where does cluster 4711 live" cannot
//! drift apart.
//!
//! Reads and writes take `&self`, like the device beneath them. A checker that
//! reads the whole FAT and a writer that updates one entry can therefore run
//! against the same volume without either of them owning it.

use crate::device::BlockDevice;
use crate::error::{Error, Result};
use crate::fat;
use crate::params::FatType;
use crate::structs::{BootSector, FsInfo};

/// A FAT filesystem on a device.
///
/// `Debug` prints the geometry rather than the device, which may be a whole
/// image held in memory.
pub struct Filesystem<D: BlockDevice> {
    device: D,
    boot: BootSector,
    fat_type: FatType,
}

impl<D: BlockDevice> std::fmt::Debug for Filesystem<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Filesystem")
            .field("fat_type", &self.fat_type)
            .field("cluster_size", &self.cluster_size())
            .field("cluster_count", &self.cluster_count())
            .field("label", &self.boot.label())
            .finish()
    }
}

impl<D: BlockDevice> Filesystem<D> {
    /// Open a filesystem by reading its boot sector.
    ///
    /// Fails if what is there is not a FAT filesystem. It does not check the
    /// filesystem — see [`crate::fsck`] for that — but it does refuse a boot
    /// sector whose geometry does not fit the device, since every offset
    /// computed afterwards would be a guess.
    pub async fn open(device: D) -> Result<Self> {
        let mut buf = vec![0u8; 512];
        device.read_at(0, &mut buf).await?;
        let boot = BootSector::decode(&buf)?;
        let fat_type = boot.fat_type();

        let end = boot.total_sectors() as u64 * boot.bytes_per_sector as u64;
        if end > device.size() {
            return Err(Error::corrupt(
                "boot sector",
                format!(
                    "claims {} sectors of {} bytes ({end} bytes) on a {}-byte device",
                    boot.total_sectors(),
                    boot.bytes_per_sector,
                    device.size()
                ),
            ));
        }
        if boot.cluster_count() == 0 {
            return Err(Error::corrupt(
                "boot sector",
                "the geometry leaves no room for a single cluster",
            ));
        }

        Ok(Self {
            device,
            boot,
            fat_type,
        })
    }

    /// The boot sector.
    pub fn boot(&self) -> &BootSector {
        &self.boot
    }

    /// The FAT width.
    pub fn fat_type(&self) -> FatType {
        self.fat_type
    }

    /// The device underneath.
    pub fn device(&self) -> &D {
        &self.device
    }

    /// Give the device back.
    pub fn into_device(self) -> D {
        self.device
    }

    /// Bytes per cluster.
    pub fn cluster_size(&self) -> u32 {
        self.boot.cluster_size()
    }

    /// Data clusters, numbered 2 through `cluster_count() + 1`.
    pub fn cluster_count(&self) -> u32 {
        self.boot.cluster_count()
    }

    /// The highest cluster number that exists on this volume.
    pub fn max_cluster(&self) -> u32 {
        self.cluster_count() + 1
    }

    /// Is `cluster` a cluster this volume actually has?
    ///
    /// Clusters 0 and 1 are the reserved FAT entries and have no storage, which
    /// is why a chain pointing at either of them is corruption rather than a
    /// short chain.
    pub fn is_data_cluster(&self, cluster: u32) -> bool {
        (2..=self.max_cluster()).contains(&cluster)
    }

    /// Byte offset of a cluster.
    pub fn cluster_offset(&self, cluster: u32) -> u64 {
        self.boot.cluster_offset(cluster)
    }

    /// Byte offset of one FAT. `index` counts from zero.
    pub fn fat_offset(&self, index: u8) -> u64 {
        let ss = self.boot.bytes_per_sector as u64;
        self.boot.reserved_sectors as u64 * ss + index as u64 * self.fat_bytes()
    }

    /// Bytes in one FAT.
    pub fn fat_bytes(&self) -> u64 {
        self.boot.fat_length() as u64 * self.boot.bytes_per_sector as u64
    }

    /// Read one whole FAT into memory.
    ///
    /// A FAT32 volume's FAT can be megabytes, so this is a deliberate choice a
    /// caller makes rather than something the entry accessors do behind its
    /// back — but a checker that walks every chain wants it exactly once.
    pub async fn read_fat(&self, index: u8) -> Result<Vec<u8>> {
        if index >= self.boot.num_fats {
            return Err(Error::invalid(format!(
                "FAT {index} does not exist; the volume has {}",
                self.boot.num_fats
            )));
        }
        let mut buf = vec![0u8; self.fat_bytes() as usize];
        self.device.read_at(self.fat_offset(index), &mut buf).await?;
        Ok(buf)
    }

    /// Read one FAT entry from the device.
    ///
    /// Reads only the bytes the entry occupies. A FAT12 entry straddles two
    /// bytes and, at the end of a sector, two sectors — reading by byte offset
    /// rather than by sector is what keeps that from being a special case.
    pub async fn fat_entry(&self, cluster: u32) -> Result<u32> {
        self.check_cluster(cluster)?;
        let span = fat::entry_span(self.fat_type);
        let off = self.fat_offset(0) + fat::entry_offset(self.fat_type, cluster);
        let mut buf = [0u8; 4];
        let n = span.min(self.fat_bytes() - fat::entry_offset(self.fat_type, cluster)) as usize;
        self.device.read_at(off, &mut buf[..n]).await?;
        Ok(match self.fat_type {
            FatType::Fat12 => fat::get_entry12_split(buf[0], buf[1], cluster),
            FatType::Fat16 => u16::from_le_bytes([buf[0], buf[1]]) as u32,
            FatType::Fat32 => u32::from_le_bytes(buf) & FatType::Fat32.mask(),
        })
    }

    /// Write one FAT entry, to every FAT.
    ///
    /// Every FAT, always. The second FAT is not a backup a driver falls back
    /// to — it is a copy every driver assumes is current, and a volume whose
    /// FATs disagree is one `fsck.fat` will report and some drivers will
    /// silently pick the wrong half of.
    pub async fn set_fat_entry(&self, cluster: u32, value: u32) -> Result<()> {
        self.check_cluster(cluster)?;
        let entry_off = fat::entry_offset(self.fat_type, cluster);
        let span = fat::entry_span(self.fat_type).min(self.fat_bytes() - entry_off) as usize;

        for index in 0..self.boot.num_fats {
            let off = self.fat_offset(index) + entry_off;
            let mut buf = [0u8; 4];
            self.device.read_at(off, &mut buf[..span]).await?;
            match self.fat_type {
                FatType::Fat12 => {
                    let (low, high) = fat::set_entry12_split(buf[0], buf[1], cluster, value);
                    buf[0] = low;
                    buf[1] = high;
                }
                FatType::Fat16 => buf[..2].copy_from_slice(&(value as u16).to_le_bytes()),
                FatType::Fat32 => {
                    let existing = u32::from_le_bytes(buf);
                    let merged = (existing & 0xf000_0000) | (value & FatType::Fat32.mask());
                    buf = merged.to_le_bytes();
                }
            }
            self.device.write_at(off, &buf[..span]).await?;
        }
        Ok(())
    }

    /// Follow a cluster chain from `start`, returning every cluster in it.
    ///
    /// Stops at the end-of-chain marker. A chain that points outside the volume
    /// or loops back on itself is corruption, and is reported rather than
    /// followed — a loop would otherwise read for ever.
    pub async fn chain(&self, start: u32) -> Result<Vec<u32>> {
        let mut chain = Vec::new();
        let mut cluster = start;
        let limit = self.cluster_count() as usize + 2;

        while self.is_data_cluster(cluster) {
            chain.push(cluster);
            if chain.len() > limit {
                return Err(Error::corrupt(
                    "cluster chain",
                    format!("chain from cluster {start} does not end"),
                ));
            }
            let next = self.fat_entry(cluster).await?;
            if self.fat_type.is_end_of_chain(next) {
                return Ok(chain);
            }
            if self.fat_type.is_bad(next) {
                return Err(Error::corrupt(
                    "cluster chain",
                    format!("cluster {cluster} points at the bad-cluster marker"),
                ));
            }
            if !self.is_data_cluster(next) {
                return Err(Error::corrupt(
                    "cluster chain",
                    format!(
                        "cluster {cluster} points at {next}, outside the 2..={} the volume has",
                        self.max_cluster()
                    ),
                ));
            }
            cluster = next;
        }

        if chain.is_empty() && start != 0 {
            return Err(Error::corrupt(
                "cluster chain",
                format!("chain starts at {start}, which is not a data cluster"),
            ));
        }
        Ok(chain)
    }

    /// Read a whole cluster.
    pub async fn read_cluster(&self, cluster: u32, buf: &mut [u8]) -> Result<()> {
        self.check_data_cluster(cluster)?;
        let len = self.cluster_size() as usize;
        if buf.len() < len {
            return Err(Error::invalid(format!(
                "a cluster is {len} bytes; the buffer holds {}",
                buf.len()
            )));
        }
        self.device
            .read_at(self.cluster_offset(cluster), &mut buf[..len])
            .await
    }

    /// Write a whole cluster.
    pub async fn write_cluster(&self, cluster: u32, data: &[u8]) -> Result<()> {
        self.check_data_cluster(cluster)?;
        let len = self.cluster_size() as usize;
        if data.len() != len {
            return Err(Error::invalid(format!(
                "a cluster is {len} bytes; the data is {}",
                data.len()
            )));
        }
        self.device.write_at(self.cluster_offset(cluster), data).await
    }

    /// Read a directory's bytes: the fixed root when `start` is `None`,
    /// otherwise the chain beginning at `start`.
    ///
    /// FAT12 and FAT16 keep the root directory in a fixed area with no chain
    /// behind it, which is why it has a size limit and why this takes an
    /// `Option` rather than a cluster number.
    pub async fn read_directory(&self, start: Option<u32>) -> Result<Vec<u8>> {
        // The fixed root has no chain to follow; everything else does, the
        // FAT32 root included, which is why the root cluster is substituted
        // here rather than handled as its own case.
        let start = match start {
            None if self.fat_type != FatType::Fat32 => {
                let ss = self.boot.bytes_per_sector as u64;
                let len = self.boot.root_dir_sectors() as u64 * ss;
                let mut buf = vec![0u8; len as usize];
                self.device.read_at(self.boot.root_dir_offset(), &mut buf).await?;
                return Ok(buf);
            }
            None => self.boot.root_cluster,
            Some(start) => start,
        };
        let chain = self.chain(start).await?;
        let cluster_size = self.cluster_size() as usize;
        let mut buf = vec![0u8; chain.len() * cluster_size];
        for (i, &cluster) in chain.iter().enumerate() {
            self.device
                .read_at(
                    self.cluster_offset(cluster),
                    &mut buf[i * cluster_size..(i + 1) * cluster_size],
                )
                .await?;
        }
        Ok(buf)
    }

    /// Read the FSInfo sector. `None` on FAT12 and FAT16, which have none.
    pub async fn read_fsinfo(&self) -> Result<Option<FsInfo>> {
        if self.fat_type != FatType::Fat32 || self.boot.info_sector == 0 {
            return Ok(None);
        }
        let mut buf = vec![0u8; 512];
        let off = self.boot.sector_offset(self.boot.info_sector as u64);
        self.device.read_at(off, &mut buf).await?;
        FsInfo::decode(&buf).map(Some)
    }

    /// Write the FSInfo sector, and its backup copy when there is one.
    ///
    /// Both copies or neither: a driver reading the backup after a bad shutdown
    /// should not find a count older than the one it just replaced.
    pub async fn write_fsinfo(&self, info: &FsInfo) -> Result<()> {
        if self.fat_type != FatType::Fat32 || self.boot.info_sector == 0 {
            return Ok(());
        }
        let mut buf = vec![0u8; self.boot.bytes_per_sector as usize];
        info.encode(&mut buf);
        let off = self.boot.sector_offset(self.boot.info_sector as u64);
        self.device.write_at(off, &buf).await?;

        let backup = self.boot.backup_boot as u32 + self.boot.info_sector as u32;
        if self.boot.backup_boot != 0 && backup < self.boot.reserved_sectors as u32 {
            self.device
                .write_at(self.boot.sector_offset(backup as u64), &buf)
                .await?;
        }
        Ok(())
    }

    /// Flush the device.
    pub async fn flush(&self) -> Result<()> {
        self.device.flush().await
    }

    fn check_cluster(&self, cluster: u32) -> Result<()> {
        if cluster > self.max_cluster() {
            return Err(Error::invalid(format!(
                "cluster {cluster} is past the last cluster ({})",
                self.max_cluster()
            )));
        }
        Ok(())
    }

    fn check_data_cluster(&self, cluster: u32) -> Result<()> {
        if !self.is_data_cluster(cluster) {
            return Err(Error::invalid(format!(
                "cluster {cluster} is not a data cluster (the volume has 2..={})",
                self.max_cluster()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MemDevice;
    use crate::format::format;
    use crate::params::{FatType, Params};

    const MIB: u64 = 1024 * 1024;

    async fn fresh(size: u64, params: &Params) -> Filesystem<MemDevice> {
        let dev = MemDevice::new(size);
        format(&dev, params).await.unwrap();
        Filesystem::open(dev).await.unwrap()
    }

    #[tokio::test]
    async fn opens_what_the_formatter_wrote() {
        for (size, want) in [
            (8 * MIB, FatType::Fat12),
            (64 * MIB, FatType::Fat16),
            (1024 * MIB, FatType::Fat32),
        ] {
            let fs = fresh(size, &Params::new()).await;
            assert_eq!(fs.fat_type(), want, "at {size} bytes");
            assert!(fs.cluster_count() > 0);
            // Entry 0 carries the media byte; entry 1 is all ones.
            assert_eq!(fs.fat_entry(0).await.unwrap() & 0xff, fs.boot().media as u32);
            assert!(want.is_end_of_chain(fs.fat_entry(1).await.unwrap()));
        }
    }

    #[tokio::test]
    async fn a_written_entry_reaches_every_fat() {
        let fs = fresh(64 * MIB, &Params::new()).await;
        fs.set_fat_entry(5, 6).await.unwrap();
        assert_eq!(fs.fat_entry(5).await.unwrap(), 6);

        let fat0 = fs.read_fat(0).await.unwrap();
        let fat1 = fs.read_fat(1).await.unwrap();
        assert_eq!(fat0, fat1, "the FATs disagree after a write");
        assert_eq!(fat::get_entry(&fat0, fs.fat_type(), 5), 6);
    }

    /// FAT12 is where an entry can straddle a byte *and* a sector. Writing a
    /// long run and reading it back through the device is the only way to catch
    /// a helper that quietly assumes an entry fits in one sector.
    #[tokio::test]
    async fn fat12_entries_survive_the_sector_boundary() {
        let fs = fresh(8 * MIB, &Params::new()).await;
        assert_eq!(fs.fat_type(), FatType::Fat12);

        let sector_entries = fs.boot().bytes_per_sector as u32 * 8 / 12;
        let around = sector_entries - 2..sector_entries + 3;
        for cluster in around.clone() {
            fs.set_fat_entry(cluster, cluster + 100).await.unwrap();
        }
        for cluster in around {
            assert_eq!(
                fs.fat_entry(cluster).await.unwrap(),
                cluster + 100,
                "cluster {cluster}"
            );
        }
    }

    #[tokio::test]
    async fn a_chain_is_followed_to_its_end() {
        let fs = fresh(64 * MIB, &Params::new()).await;
        for c in 10..20 {
            fs.set_fat_entry(c, c + 1).await.unwrap();
        }
        fs.set_fat_entry(20, fs.fat_type().eof_marker()).await.unwrap();
        assert_eq!(fs.chain(10).await.unwrap(), (10..=20).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn a_chain_that_loops_is_reported_not_followed() {
        let fs = fresh(64 * MIB, &Params::new()).await;
        fs.set_fat_entry(10, 11).await.unwrap();
        fs.set_fat_entry(11, 10).await.unwrap();
        let err = fs.chain(10).await.unwrap_err();
        assert!(matches!(err, Error::Corrupt { .. }), "{err}");
    }

    #[tokio::test]
    async fn a_chain_pointing_off_the_volume_is_reported() {
        let fs = fresh(64 * MIB, &Params::new()).await;
        let past_the_end = fs.max_cluster();
        fs.set_fat_entry(10, past_the_end).await.unwrap();
        fs.set_fat_entry(past_the_end, past_the_end + 1).await.unwrap();
        let err = fs.chain(10).await.unwrap_err();
        assert!(matches!(err, Error::Corrupt { .. }), "{err}");
    }

    #[tokio::test]
    async fn the_root_directory_reads_back_on_every_width() {
        for (size, len_check) in [(8 * MIB, true), (64 * MIB, true), (1024 * MIB, false)] {
            let fs = fresh(size, &Params::new().label("ROOT")).await;
            let dir = fs.read_directory(None).await.unwrap();
            if len_check {
                assert_eq!(
                    dir.len() as u32,
                    fs.boot().root_entries as u32 * 32,
                    "root directory length"
                );
            } else {
                assert_eq!(dir.len() as u32, fs.cluster_size());
            }
            let label = crate::structs::DirEntry::decode(&dir);
            assert!(label.is_volume_label());
            assert_eq!(label.short_name(), "ROOT");
        }
    }

    #[tokio::test]
    async fn the_fsinfo_sector_round_trips_on_fat32_and_is_absent_elsewhere() {
        let fs = fresh(1024 * MIB, &Params::new()).await;
        let mut info = fs.read_fsinfo().await.unwrap().unwrap();
        assert_eq!(info.free_clusters, fs.cluster_count() - 1);
        info.free_clusters -= 10;
        fs.write_fsinfo(&info).await.unwrap();
        assert_eq!(fs.read_fsinfo().await.unwrap().unwrap(), info);

        let fs = fresh(64 * MIB, &Params::new()).await;
        assert!(fs.read_fsinfo().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_device_that_is_not_a_filesystem_is_refused() {
        let dev = MemDevice::new(1024 * 1024);
        let err = Filesystem::open(dev).await.unwrap_err();
        assert!(matches!(err, Error::NotFatFilesystem { .. }), "{err}");
    }

    #[tokio::test]
    async fn a_boot_sector_claiming_more_than_the_device_holds_is_refused() {
        let dev = MemDevice::new(64 * MIB);
        format(&dev, &Params::new()).await.unwrap();
        // Claim twice the sectors the device has.
        let mut image = dev.to_vec();
        let doubled = (64 * MIB / 512 * 2) as u32;
        image[32..36].copy_from_slice(&doubled.to_le_bytes());
        image[19..21].copy_from_slice(&0u16.to_le_bytes());
        let dev = MemDevice::from_vec(image);

        let err = Filesystem::open(dev).await.unwrap_err();
        assert!(matches!(err, Error::Corrupt { .. }), "{err}");
    }
}
