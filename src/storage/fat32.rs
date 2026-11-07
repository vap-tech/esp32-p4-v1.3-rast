//! Small FAT32 reader and create-new streaming writer. Metadata cache and partial-sector data cache
//! are separate; aligned file data bypasses both and uses multi-sector I/O.
use super::{BlockDevice, Error};
use core::fmt;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub sectors: u32,
    pub sectors_per_cluster: u8,
    pub clusters: u32,
    pub root_cluster: u32,
    pub fat_start: u32,
    pub data_start: u32,
    fat_base: u32,
    fat_size: u32,
    fat_copies: u8,
    mirrored: bool,
    fsinfo: Option<u32>,
    backup_fsinfo: Option<u32>,
}
impl Geometry {
    fn parse<E>(boot: &[u8; 512], available: u64) -> Result<Self, Error<E>> {
        if boot[510..] != [0x55, 0xaa] {
            return Err(Error::Invalid("FAT boot signature"));
        }
        if le16(boot, 11) != 512 {
            return Err(Error::Unsupported("sector size"));
        }
        let spc = boot[13];
        let reserved = le16(boot, 14) as u32;
        let fats = boot[16] as u32;
        let fat_size = le32(boot, 36);
        let sectors = le32(boot, 32);
        if !spc.is_power_of_two()
            || spc > 128
            || reserved == 0
            || !(1..=2).contains(&fats)
            || fat_size == 0
            || sectors == 0
        {
            return Err(Error::Invalid("FAT32 geometry"));
        }
        if le16(boot, 17) != 0 || le16(boot, 19) != 0 || le16(boot, 22) != 0 || le16(boot, 42) != 0
        {
            return Err(Error::Unsupported("not FAT32 v0"));
        }
        let data_start = u64::from(reserved) + u64::from(fats) * u64::from(fat_size);
        if u64::from(sectors) > available || data_start >= u64::from(sectors) {
            return Err(Error::Invalid("FAT32 volume bounds"));
        }
        let clusters = ((u64::from(sectors) - data_start) / u64::from(spc)) as u32;
        if !(65_525..=0x0fff_ffee).contains(&clusters)
            || u64::from(fat_size) * 128 < u64::from(clusters) + 2
        {
            return Err(Error::Invalid("FAT32 cluster count/table size"));
        }
        let root_cluster = le32(boot, 44);
        if !(2..clusters + 2).contains(&root_cluster) {
            return Err(Error::Invalid("root cluster"));
        }
        let flags = le16(boot, 40);
        let active = if flags & 0x80 != 0 {
            (flags & 15) as u32
        } else {
            0
        };
        if active >= fats {
            return Err(Error::Invalid("active FAT index"));
        }
        let info = u32::from(le16(boot, 48));
        let backup = u32::from(le16(boot, 50));
        let fsinfo = if info == 0 || info == 0xffff {
            None
        } else {
            if info >= reserved {
                return Err(Error::Invalid("FSInfo bounds"));
            }
            Some(info)
        };
        let backup_fsinfo = fsinfo.and_then(|info| {
            if backup != 0 && backup != 0xffff && backup + info < reserved {
                Some(backup + info)
            } else {
                None
            }
        });
        Ok(Self {
            sectors,
            sectors_per_cluster: spc,
            clusters,
            root_cluster,
            fat_start: reserved + active * fat_size,
            data_start: data_start as u32,
            fat_base: reserved,
            fat_size,
            fat_copies: fats as u8,
            mirrored: flags & 0x80 == 0,
            fsinfo,
            backup_fsinfo,
        })
    }
    fn valid_cluster(&self, cluster: u32) -> bool {
        (2..self.clusters + 2).contains(&cluster)
    }
    fn cluster_sector(&self, cluster: u32) -> u32 {
        self.data_start + (cluster - 2) * u32::from(self.sectors_per_cluster)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShortName([u8; 11]);
impl ShortName {
    fn parse<E>(name: &str) -> Result<Self, Error<E>> {
        let mut raw = [b' '; 11];
        let mut base = 0;
        let mut ext = 0;
        let mut dotted = false;
        for byte in name.bytes() {
            if byte == b'.' && !dotted && base != 0 {
                dotted = true;
                continue;
            }
            if !byte.is_ascii_graphic() || b"\"*+,./:;<=>?[\\]|".contains(&byte) {
                return Err(Error::InvalidName);
            }
            if dotted {
                if ext == 3 {
                    return Err(Error::InvalidName);
                }
                raw[8 + ext] = byte.to_ascii_uppercase();
                ext += 1;
            } else {
                if base == 8 {
                    return Err(Error::InvalidName);
                }
                raw[base] = byte.to_ascii_uppercase();
                base += 1;
            }
        }
        if base == 0 || (dotted && ext == 0) {
            return Err(Error::InvalidName);
        }
        Ok(Self(raw))
    }
}
impl fmt::Display for ShortName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (part, bytes) in [self.0[..8].trim_ascii_end(), self.0[8..].trim_ascii_end()]
            .iter()
            .enumerate()
        {
            if part == 1 && !bytes.is_empty() {
                f.write_str(".")?;
            }
            for &byte in *bytes {
                write!(
                    f,
                    "{}",
                    if byte.is_ascii_graphic() {
                        byte as char
                    } else {
                        '?'
                    }
                )?;
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug)]
pub struct DirEntry {
    pub name: ShortName,
    pub size: u32,
    pub attributes: u8,
    cluster: u32,
}
impl DirEntry {
    pub fn is_directory(&self) -> bool {
        self.attributes & 0x10 != 0
    }
    fn decode(raw: &[u8]) -> Self {
        let mut name: [u8; 11] = raw[..11].try_into().unwrap();
        if name[0] == 0x05 {
            name[0] = 0xe5;
        }
        Self {
            name: ShortName(name),
            size: le32(raw, 28),
            attributes: raw[11],
            cluster: (u32::from(le16(raw, 20)) << 16) | u32::from(le16(raw, 26)),
        }
    }
}

struct Cache {
    sector: Option<u32>,
    bytes: [u8; 512],
}
impl Cache {
    fn new() -> Self {
        Self {
            sector: None,
            bytes: [0; 512],
        }
    }
    fn read<D: BlockDevice>(&mut self, device: &mut D, sector: u32) -> Result<(), Error<D::Error>> {
        if self.sector != Some(sector) {
            self.sector = None;
            device
                .read(u64::from(sector), &mut self.bytes)
                .map_err(Error::Device)?;
            self.sector = Some(sector);
        }
        Ok(())
    }
}

pub struct Fat32<D> {
    device: D,
    geometry: Geometry,
    metadata: Cache,
    edge: Cache,
    write_failed: bool,
    next_free: u32,
}
impl<D: BlockDevice> Fat32<D> {
    /// Device sector zero must be the FAT32 boot sector (e.g. a Partition view).
    pub fn mount(mut device: D) -> Result<Self, Error<D::Error>> {
        if device.max_transfer_bytes() == 0 || !device.max_transfer_bytes().is_multiple_of(512) {
            return Err(Error::Invalid("block transfer limit"));
        }
        let mut boot = [0; 512];
        device.read(0, &mut boot).map_err(Error::Device)?;
        let geometry = Geometry::parse(&boot, device.sectors())?;
        Ok(Self {
            device,
            geometry,
            metadata: Cache::new(),
            edge: Cache::new(),
            write_failed: false,
            next_free: 2,
        })
    }
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }
    pub fn into_inner(self) -> D {
        self.device
    }
    fn next_cluster(&mut self, cluster: u32) -> Result<Option<u32>, Error<D::Error>> {
        if !self.geometry.valid_cluster(cluster) {
            return Err(Error::Invalid("cluster bounds"));
        }
        let sector = self.geometry.fat_start + cluster / 128;
        self.metadata.read(&mut self.device, sector)?;
        let next = le32(&self.metadata.bytes, (cluster % 128) as usize * 4) & 0x0fff_ffff;
        if next >= 0x0fff_fff8 {
            return Ok(None);
        }
        if !self.geometry.valid_cluster(next) || next == cluster {
            return Err(Error::Invalid("broken FAT chain"));
        }
        Ok(Some(next))
    }
    fn required_next(&mut self, cluster: u32) -> Result<u32, Error<D::Error>> {
        self.next_cluster(cluster)?
            .ok_or(Error::Invalid("premature end of FAT chain"))
    }
    /// Callback returns false to stop. Entries are streamed, never collected.
    fn scan(
        &mut self,
        mut cluster: u32,
        mut callback: impl FnMut(DirEntry) -> bool,
    ) -> Result<(), Error<D::Error>> {
        for _ in 0..self.geometry.clusters {
            if !self.geometry.valid_cluster(cluster) {
                return Err(Error::Invalid("directory cluster"));
            }
            let first = self.geometry.cluster_sector(cluster);
            for sector in first..first + u32::from(self.geometry.sectors_per_cluster) {
                self.metadata.read(&mut self.device, sector)?;
                for raw in self.metadata.bytes.as_chunks::<32>().0 {
                    if raw[0] == 0 {
                        return Ok(());
                    }
                    if raw[0] == 0xe5 || raw[11] & 0x08 != 0 {
                        continue;
                    } // Deleted, LFN, volume label.
                    if !callback(DirEntry::decode(raw)) {
                        return Ok(());
                    }
                }
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => return Ok(()),
            }
        }
        Err(Error::Invalid("directory chain exceeds cluster count"))
    }
    fn lookup(&mut self, cluster: u32, name: ShortName) -> Result<DirEntry, Error<D::Error>> {
        let mut found = None;
        self.scan(cluster, |entry| {
            if entry.name.0.eq_ignore_ascii_case(&name.0) {
                found = Some(entry);
                false
            } else {
                true
            }
        })?;
        found.ok_or(Error::NotFound)
    }
    fn resolve(&mut self, path: &str) -> Result<DirEntry, Error<D::Error>> {
        let mut entry = DirEntry {
            name: ShortName(*b"/          "),
            size: 0,
            attributes: 0x10,
            cluster: self.geometry.root_cluster,
        };
        for name in path.split('/').filter(|s| !s.is_empty() && *s != ".") {
            if name == ".." {
                return Err(Error::Unsupported("parent path components"));
            }
            if !entry.is_directory() {
                return Err(Error::NotDirectory);
            }
            entry = self.lookup(entry.cluster, ShortName::parse(name)?)?;
        }
        if path.ends_with('/') && !entry.is_directory() {
            return Err(Error::NotDirectory);
        }
        Ok(entry)
    }
    pub fn read_dir(
        &mut self,
        path: &str,
        callback: impl FnMut(DirEntry) -> bool,
    ) -> Result<(), Error<D::Error>> {
        let entry = self.resolve(path)?;
        if !entry.is_directory() {
            return Err(Error::NotDirectory);
        }
        self.scan(entry.cluster, callback)
    }
    /// Synchronously create a new ASCII 8.3 file. Existing files are never
    /// overwritten. Data/FAT are persisted before publishing the directory entry.
    /// An I/O failure leaves the volume dirty and disables further creates until
    /// recovery/remount; this is ordered creation, not a journaled transaction.
    pub fn create_new(&mut self, path: &str, contents: &[u8]) -> Result<(), Error<D::Error>> {
        u32::try_from(contents.len()).map_err(|_| Error::Unsupported("file size"))?;
        let mut writer = self.create_stream(path)?;
        writer.write(contents)?;
        writer.finish()
    }
    /// Start a create-new stream. Only finish publishes the directory entry.
    /// Drop performs no I/O and leaves the volume dirty; call abort explicitly
    /// on a clean cancellation. Device errors require recovery, not rollback.
    pub fn create_stream(&mut self, path: &str) -> Result<FileWriter<'_, D>, Error<D::Error>> {
        if self.write_failed {
            return Err(Error::NeedsRecovery);
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("/", path));
        let name = ShortName::parse(name)?;
        let directory = self.resolve(parent)?;
        if !directory.is_directory() {
            return Err(Error::NotDirectory);
        }
        match self.lookup(directory.cluster, name) {
            Ok(_) => return Err(Error::AlreadyExists),
            Err(Error::NotFound) => {}
            Err(error) => return Err(error),
        }
        let slot = self.free_slot(directory.cluster)?;
        let status = self.write_status()?;
        self.write_failed = true;
        self.set_fat(1, status & !(1 << 27))?;
        self.invalidate_fsinfo()?;
        Ok(FileWriter {
            fs: self,
            slot,
            name,
            status,
            first: 0,
            current: 0,
            offset: 0,
            size: 0,
            tail: [0; 512],
            used: 0,
            failed: false,
        })
    }
    fn publish_file(
        &mut self,
        slot: &DirectorySlot,
        name: ShortName,
        first: u32,
        size: u32,
    ) -> Result<(), Error<D::Error>> {
        // Preserve the end marker when replacing the first unused directory slot.
        if slot.was_end {
            if let Some((sector, offset)) = slot.following {
                self.metadata.read(&mut self.device, sector)?;
                self.metadata.bytes[offset] = 0;
                self.write_metadata(sector)?;
            }
        }
        self.metadata.read(&mut self.device, slot.sector)?;
        let raw = &mut self.metadata.bytes[slot.offset..slot.offset + 32];
        raw.fill(0);
        raw[..11].copy_from_slice(&name.0);
        raw[11] = 0x20; // Archive, ordinary file.
                        // The board RTC is not valid. Use the minimum valid FAT date, 1980-01-01.
        for at in [16, 18, 24] {
            raw[at..at + 2].copy_from_slice(&0x21u16.to_le_bytes());
        }
        raw[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        raw[26..28].copy_from_slice(&(first as u16).to_le_bytes());
        raw[28..32].copy_from_slice(&size.to_le_bytes());
        self.write_metadata(slot.sector)?;
        Ok(())
    }
    fn write_status(&mut self) -> Result<u32, Error<D::Error>> {
        let status = self.fat_value(1)?;
        if status & 0x0c00_0000 != 0x0c00_0000 {
            return Err(Error::NeedsRecovery);
        }
        // Final clean-flag writes can fail between FAT copies. Do not accept a
        // clean primary with a dirty mirror after a remount.
        if self.geometry.mirrored {
            for copy in 0..self.geometry.fat_copies {
                let sector = self.geometry.fat_base + u32::from(copy) * self.geometry.fat_size;
                self.metadata.read(&mut self.device, sector)?;
                if le32(&self.metadata.bytes, 4) & 0x0c00_0000 != 0x0c00_0000 {
                    return Err(Error::NeedsRecovery);
                }
            }
        }
        Ok(status)
    }
    fn fat_value(&mut self, cluster: u32) -> Result<u32, Error<D::Error>> {
        self.metadata
            .read(&mut self.device, self.geometry.fat_start + cluster / 128)?;
        Ok(le32(&self.metadata.bytes, (cluster % 128) as usize * 4) & 0x0fff_ffff)
    }
    fn write_metadata(&mut self, sector: u32) -> Result<(), Error<D::Error>> {
        self.metadata.sector = None;
        self.edge.sector = None;
        self.device
            .write(u64::from(sector), &self.metadata.bytes)
            .map_err(Error::Device)?;
        self.metadata.sector = Some(sector);
        Ok(())
    }
    fn set_fat(&mut self, cluster: u32, value: u32) -> Result<(), Error<D::Error>> {
        let g = self.geometry;
        for copy in 0..g.fat_copies {
            let start = g.fat_base + u32::from(copy) * g.fat_size;
            if !g.mirrored && start != g.fat_start {
                continue;
            }
            let sector = start + cluster / 128;
            self.metadata.read(&mut self.device, sector)?;
            let offset = (cluster % 128) as usize * 4;
            let old = le32(&self.metadata.bytes, offset);
            let updated = (old & 0xf000_0000) | (value & 0x0fff_ffff);
            self.metadata.bytes[offset..offset + 4].copy_from_slice(&updated.to_le_bytes());
            self.write_metadata(sector)?;
        }
        Ok(())
    }
    fn invalidate_fsinfo(&mut self) -> Result<(), Error<D::Error>> {
        for sector in [self.geometry.fsinfo, self.geometry.backup_fsinfo]
            .into_iter()
            .flatten()
        {
            self.metadata.read(&mut self.device, sector)?;
            let bytes = &mut self.metadata.bytes;
            if le32(bytes, 0) != 0x4161_5252
                || le32(bytes, 484) != 0x6141_7272
                || le32(bytes, 508) != 0xaa55_0000
            {
                continue;
            }
            let hint = le32(bytes, 492);
            if Some(sector) == self.geometry.fsinfo && self.geometry.valid_cluster(hint) {
                self.next_free = hint;
            }
            // Unknown is valid per FAT32; never leave stale free-space counts,
            // including in the backup FSInfo. A host can recompute them later.
            bytes[488..496].fill(0xff);
            self.write_metadata(sector)?;
        }
        Ok(())
    }
    fn allocate(&mut self, previous: Option<u32>) -> Result<u32, Error<D::Error>> {
        let mut cluster = self.next_free;
        for _ in 0..self.geometry.clusters {
            if self.fat_value(cluster)? == 0 {
                self.set_fat(cluster, 0x0fff_ffff)?;
                if let Some(previous) = previous {
                    self.set_fat(previous, cluster)?;
                }
                self.next_free = if cluster + 1 < self.geometry.clusters + 2 {
                    cluster + 1
                } else {
                    2
                };
                return Ok(cluster);
            }
            cluster = if cluster + 1 < self.geometry.clusters + 2 {
                cluster + 1
            } else {
                2
            };
        }
        Err(Error::NoSpace)
    }
    fn free_slot(&mut self, mut cluster: u32) -> Result<DirectorySlot, Error<D::Error>> {
        for _ in 0..self.geometry.clusters {
            if !self.geometry.valid_cluster(cluster) {
                return Err(Error::Invalid("directory cluster"));
            }
            let first = self.geometry.cluster_sector(cluster);
            let end = first + u32::from(self.geometry.sectors_per_cluster);
            for sector in first..end {
                self.metadata.read(&mut self.device, sector)?;
                for offset in (0..512).step_by(32) {
                    let first_byte = self.metadata.bytes[offset];
                    if first_byte != 0 && first_byte != 0xe5 {
                        continue;
                    }
                    let following = if offset < 480 {
                        Some((sector, offset + 32))
                    } else if sector + 1 < end {
                        Some((sector + 1, 0))
                    } else if first_byte == 0 {
                        self.next_cluster(cluster)?
                            .map(|next| (self.geometry.cluster_sector(next), 0))
                    } else {
                        None
                    };
                    return Ok(DirectorySlot {
                        sector,
                        offset,
                        was_end: first_byte == 0,
                        following,
                    });
                }
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => return Err(Error::DirectoryFull),
            }
        }
        Err(Error::Invalid("directory chain exceeds cluster count"))
    }
    /// A file borrows its volume exclusively. No heap, handles table or Drop I/O.
    pub fn open(&mut self, path: &str) -> Result<File<'_, D>, Error<D::Error>> {
        let entry = self.resolve(path)?;
        if entry.is_directory() {
            return Err(Error::NotFile);
        }
        if entry.size != 0 && !self.geometry.valid_cluster(entry.cluster) {
            return Err(Error::Invalid("file first cluster"));
        }
        if u64::from(entry.size)
            > u64::from(self.geometry.clusters) * u64::from(self.geometry.sectors_per_cluster) * 512
        {
            return Err(Error::Invalid("file exceeds data region"));
        }
        Ok(File {
            fs: self,
            entry,
            position: 0,
            hint_index: 0,
            hint_cluster: entry.cluster,
        })
    }
}

struct DirectorySlot {
    sector: u32,
    offset: usize,
    was_end: bool,
    following: Option<(u32, usize)>,
}

/// Fixed 512-byte edge buffer, independent of file size. No heap or Drop I/O.
/// Complete sectors bypass it and are sent in device-sized multi-sector writes.
#[must_use = "finish publishes the file; abort cancels; dropping leaves the volume dirty"]
pub struct FileWriter<'a, D> {
    fs: &'a mut Fat32<D>,
    slot: DirectorySlot,
    name: ShortName,
    status: u32,
    first: u32,
    current: u32,
    offset: u32,
    size: u32,
    tail: [u8; 512],
    used: usize,
    failed: bool,
}
impl<D: BlockDevice> FileWriter<'_, D> {
    pub fn len(&self) -> u32 {
        self.size
    }
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
    /// Accept arbitrary chunk boundaries. On any error the stream is poisoned:
    /// do not retry that chunk or finish; the volume remains dirty for recovery.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), Error<D::Error>> {
        if self.failed {
            return Err(Error::NeedsRecovery);
        }
        let result = self.write_inner(bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn write_inner(&mut self, mut bytes: &[u8]) -> Result<(), Error<D::Error>> {
        if bytes.len() as u64 + u64::from(self.size) > u64::from(u32::MAX) {
            return Err(Error::Unsupported("file size"));
        }
        let cluster_bytes = u32::from(self.fs.geometry.sectors_per_cluster) * 512;
        while !bytes.is_empty() {
            if self.current == 0 || self.offset == cluster_bytes {
                self.current = self
                    .fs
                    .allocate((self.current != 0).then_some(self.current))?;
                if self.first == 0 {
                    self.first = self.current;
                }
                self.offset = 0;
            }
            let sector = self.fs.geometry.cluster_sector(self.current) + self.offset / 512;
            self.fs.edge.sector = None;
            if self.used != 0 || bytes.len() < 512 {
                let n = (512 - self.used).min(bytes.len());
                self.tail[self.used..self.used + n].copy_from_slice(&bytes[..n]);
                self.used += n;
                self.size += n as u32;
                bytes = &bytes[n..];
                if self.used == 512 {
                    self.fs
                        .device
                        .write(u64::from(sector), &self.tail)
                        .map_err(Error::Device)?;
                    self.offset += 512;
                    self.used = 0;
                    self.tail.fill(0);
                }
            } else {
                let n = bytes
                    .len()
                    .min((cluster_bytes - self.offset) as usize)
                    .min(self.fs.device.max_transfer_bytes())
                    / 512
                    * 512;
                self.fs
                    .device
                    .write(u64::from(sector), &bytes[..n])
                    .map_err(Error::Device)?;
                self.offset += n as u32;
                self.size += n as u32;
                bytes = &bytes[n..];
            }
        }
        Ok(())
    }
    /// Flush the last edge, clear cluster slack and publish only after all data.
    /// A publication/clean-flag failure leaves the volume dirty, as create_new did.
    pub fn finish(mut self) -> Result<(), Error<D::Error>> {
        if self.failed {
            return Err(Error::NeedsRecovery);
        }
        if self.current != 0 {
            self.tail[self.used..].fill(0);
            let end = u32::from(self.fs.geometry.sectors_per_cluster) * 512;
            while self.offset < end {
                let sector = self.fs.geometry.cluster_sector(self.current) + self.offset / 512;
                self.fs
                    .device
                    .write(u64::from(sector), &self.tail)
                    .map_err(Error::Device)?;
                self.offset += 512;
                self.tail.fill(0);
            }
        }
        self.fs
            .publish_file(&self.slot, self.name, self.first, self.size)?;
        self.fs.set_fat(1, self.status)?;
        self.fs.write_failed = false;
        Ok(())
    }
    /// Cancel a healthy, unpublished stream and free its cluster chain. A failed
    /// stream cannot be rolled back because a device write may have partially
    /// changed metadata. Abort itself can fail and then leaves the volume dirty.
    pub fn abort(self) -> Result<(), Error<D::Error>> {
        if self.failed {
            return Err(Error::NeedsRecovery);
        }
        let mut cluster = self.first;
        for _ in 0..self.fs.geometry.clusters {
            if cluster == 0 {
                break;
            }
            let next = self.fs.next_cluster(cluster)?;
            self.fs.set_fat(cluster, 0)?;
            cluster = next.unwrap_or(0);
        }
        if cluster != 0 {
            return Err(Error::Invalid("abort chain too long"));
        }
        self.fs.set_fat(1, self.status)?;
        self.fs.write_failed = false;
        Ok(())
    }
}

pub struct File<'a, D> {
    fs: &'a mut Fat32<D>,
    entry: DirEntry,
    position: u32,
    hint_index: u32,
    hint_cluster: u32,
}
impl<D: BlockDevice> File<'_, D> {
    pub(crate) fn device_mut(&mut self) -> &mut D {
        &mut self.fs.device
    }
    pub fn len(&self) -> u32 {
        self.entry.size
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn position(&self) -> u32 {
        self.position
    }
    pub fn seek(&mut self, position: u32) -> Result<(), Error<D::Error>> {
        if position > self.len() {
            return Err(Error::InvalidOffset);
        }
        self.position = position;
        Ok(())
    }
    fn cluster_at(&mut self, index: u32) -> Result<u32, Error<D::Error>> {
        if index >= self.fs.geometry.clusters {
            return Err(Error::Invalid("file chain too long"));
        }
        if index < self.hint_index {
            self.hint_index = 0;
            self.hint_cluster = self.entry.cluster;
        }
        while self.hint_index < index {
            self.hint_cluster = self.fs.required_next(self.hint_cluster)?;
            self.hint_index += 1;
        }
        Ok(self.hint_cluster)
    }
    /// Direct multi-sector reads within clusters and across contiguous clusters.
    /// Only unaligned edges use the separate 512-byte data cache.
    /// On error position reflects completed chunks; discard the output buffer.
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize, Error<D::Error>> {
        let mut done = 0;
        let limit = out.len().min((self.len() - self.position) as usize);
        let geometry = self.fs.geometry;
        let spc = u32::from(geometry.sectors_per_cluster);
        while done < limit {
            let file_sector = self.position / 512;
            let index = file_sector / spc;
            let cluster = self.cluster_at(index)?;
            let in_cluster = file_sector % spc;
            let sector = geometry.cluster_sector(cluster) + in_cluster;
            let offset = (self.position % 512) as usize;
            let remaining = limit - done;
            if offset != 0 || remaining < 512 {
                self.fs.edge.read(&mut self.fs.device, sector)?;
                let n = remaining.min(512 - offset);
                out[done..done + n].copy_from_slice(&self.fs.edge.bytes[offset..offset + n]);
                done += n;
                self.position += n as u32;
                continue;
            }
            let max_sectors = remaining.min(self.fs.device.max_transfer_bytes()) / 512;
            let mut count = (spc - in_cluster).min(max_sectors as u32) as usize;
            let mut last = cluster;
            let mut last_index = index;
            while count < max_sectors {
                let next = self.fs.required_next(last)?;
                if next != last + 1 {
                    break;
                }
                count += (max_sectors - count).min(spc as usize);
                last = next;
                last_index += 1;
            }
            let bytes = count * 512;
            self.fs
                .device
                .read(u64::from(sector), &mut out[done..done + bytes])
                .map_err(Error::Device)?;
            self.hint_cluster = last;
            self.hint_index = last_index;
            done += bytes;
            self.position += bytes as u32;
        }
        Ok(done)
    }
}

/// Owns one mounted volume; it cannot be replaced while borrowed file cursors live.
pub struct SharedVolume<D> {
    inner: core::cell::RefCell<Fat32<D>>,
}
impl<D> SharedVolume<D> {
    pub fn new(volume: Fat32<D>) -> Self {
        Self {
            inner: core::cell::RefCell::new(volume),
        }
    }
    pub fn into_inner(self) -> Fat32<D> {
        self.inner.into_inner()
    }
}
/// Independent readonly cursor sharing one volume. Each operation borrows the
/// volume only for its duration; handles cannot outlive or migrate to another FS.
pub struct SharedFile<'a, D> {
    volume: &'a SharedVolume<D>,
    entry: DirEntry,
    position: u32,
    hint_index: u32,
    hint_cluster: u32,
}
impl<'a, D: BlockDevice> SharedFile<'a, D> {
    pub fn open(volume: &'a SharedVolume<D>, path: &str) -> Result<Self, Error<D::Error>> {
        let mut fs = volume.inner.borrow_mut();
        let file = fs.open(path)?;
        Ok(Self {
            volume,
            entry: file.entry,
            position: 0,
            hint_index: 0,
            hint_cluster: file.hint_cluster,
        })
    }
    pub fn len(&self) -> u32 {
        self.entry.size
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn position(&self) -> u32 {
        self.position
    }
    pub fn seek(&mut self, position: u32) -> Result<(), Error<D::Error>> {
        if position > self.len() {
            return Err(Error::InvalidOffset);
        }
        self.position = position;
        Ok(())
    }
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize, Error<D::Error>> {
        let mut fs = self.volume.inner.borrow_mut();
        let mut file = File {
            fs: &mut fs,
            entry: self.entry,
            position: self.position,
            hint_index: self.hint_index,
            hint_cluster: self.hint_cluster,
        };
        let result = file.read(out);
        // Preserve completed-chunk progress even when the transport reports an error.
        self.position = file.position;
        self.hint_index = file.hint_index;
        self.hint_cluster = file.hint_cluster;
        result
    }
    pub(crate) fn with_device<R>(&self, operation: impl FnOnce(&mut D) -> R) -> R {
        operation(&mut self.volume.inner.borrow_mut().device)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::storage::Partition;
    use std::{collections::BTreeMap, vec, vec::Vec};
    const START: u64 = 2048;
    const SPC: u32 = 8;
    const DATA: u32 = 32 + 2 * 513;
    const TOTAL: u32 = DATA + 65_525 * SPC;
    #[derive(Debug, PartialEq)]
    enum IoError {
        Bounds,
        Injected,
    }
    struct Disk {
        blocks: BTreeMap<u64, [u8; 512]>,
        requests: Vec<(u64, usize)>,
        fail_at: Option<u64>,
        writable: bool,
        fail_write: Option<u64>,
        writes: Vec<(u64, usize)>,
    }
    impl BlockDevice for Disk {
        type Error = IoError;
        fn sectors(&self) -> u64 {
            START + u64::from(TOTAL)
        }
        fn max_transfer_bytes(&self) -> usize {
            8192
        }
        fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), IoError> {
            if out.is_empty()
                || !out.len().is_multiple_of(512)
                || out.len() > 8192
                || lba + (out.len() / 512) as u64 > self.sectors()
            {
                return Err(IoError::Bounds);
            }
            self.requests.push((lba, out.len()));
            if self.fail_at == Some(lba) {
                self.fail_at = None;
                return Err(IoError::Injected);
            }
            for (i, sector) in out.as_chunks_mut::<512>().0.iter_mut().enumerate() {
                sector.copy_from_slice(self.blocks.get(&(lba + i as u64)).unwrap_or(&[0; 512]));
            }
            Ok(())
        }
        fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), IoError> {
            assert!(self.writable, "read-only filesystem wrote to disk");
            if data.is_empty()
                || !data.len().is_multiple_of(512)
                || data.len() > 8192
                || lba + (data.len() / 512) as u64 > self.sectors()
            {
                return Err(IoError::Bounds);
            }
            self.writes.push((lba, data.len()));
            if self.fail_write == Some(lba) {
                self.fail_write = None;
                return Err(IoError::Injected);
            }
            for (i, sector) in data.as_chunks::<512>().0.iter().enumerate() {
                self.blocks.insert(lba + i as u64, *sector);
            }
            Ok(())
        }
    }
    fn put16(bytes: &mut [u8], at: usize, value: u16) {
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn put32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn cluster_lba(cluster: u32) -> u64 {
        START + u64::from(DATA + (cluster - 2) * SPC)
    }
    fn fat(disk: &mut Disk, cluster: u32, next: u32) {
        let block = disk
            .blocks
            .entry(START + 32 + u64::from(cluster / 128))
            .or_insert([0; 512]);
        put32(block, (cluster % 128) as usize * 4, next);
    }
    fn entry(name: &[u8; 11], cluster: u32, size: u32, attr: u8) -> [u8; 32] {
        let mut out = [0; 32];
        out[..11].copy_from_slice(name);
        out[11] = attr;
        put16(&mut out, 20, (cluster >> 16) as u16);
        put16(&mut out, 26, cluster as u16);
        put32(&mut out, 28, size);
        out
    }
    fn pattern(i: usize) -> u8 {
        (i as u8).wrapping_mul(37) ^ ((i >> 9) as u8) ^ 0x5a
    }
    fn contents(disk: &mut Disk, clusters: &[u32]) {
        for (ci, &cluster) in clusters.iter().enumerate() {
            fat(
                disk,
                cluster,
                clusters.get(ci + 1).copied().unwrap_or(0x0fff_ffff),
            );
            for s in 0..SPC as usize {
                let sector = core::array::from_fn(|i| pattern(ci * 4096 + s * 512 + i));
                disk.blocks.insert(cluster_lba(cluster) + s as u64, sector);
            }
        }
    }
    fn disk() -> Disk {
        let mut disk = Disk {
            blocks: BTreeMap::new(),
            requests: Vec::new(),
            fail_at: None,
            writable: false,
            fail_write: None,
            writes: Vec::new(),
        };
        let mut mbr = [0; 512];
        mbr[510..].copy_from_slice(&[0x55, 0xaa]);
        mbr[450] = 0x0c;
        put32(&mut mbr, 454, START as u32);
        put32(&mut mbr, 458, TOTAL);
        disk.blocks.insert(0, mbr);
        let mut boot = [0; 512];
        boot[510..].copy_from_slice(&[0x55, 0xaa]);
        put16(&mut boot, 11, 512);
        boot[13] = SPC as u8;
        put16(&mut boot, 14, 32);
        boot[16] = 2;
        put32(&mut boot, 32, TOTAL);
        put32(&mut boot, 36, 513);
        put32(&mut boot, 44, 2);
        disk.blocks.insert(START, boot);
        fat(&mut disk, 2, 0x0fff_ffff);
        fat(&mut disk, 3, 0x0fff_ffff);
        let mut root = [0; 512];
        for (i, e) in [
            entry(b"FAST    BIN", 5, 12_165, 0x20),
            entry(b"FRAG    BIN", 10, 9000, 0x20),
            entry(b"EMPTY   TXT", 0, 0, 0x20),
            entry(b"SUB        ", 3, 0, 0x10),
            entry(b"BROKEN  BIN", 20, 8192, 0x20),
        ]
        .iter()
        .enumerate()
        {
            root[i * 32..i * 32 + 32].copy_from_slice(e);
        }
        disk.blocks.insert(cluster_lba(2), root);
        let mut sub = [0; 512];
        sub[..32].copy_from_slice(&entry(b"NESTED  BIN", 5, 12_165, 0x20));
        disk.blocks.insert(cluster_lba(3), sub);
        contents(&mut disk, &[5, 6, 7]);
        contents(&mut disk, &[10, 14, 11]);
        contents(&mut disk, &[20]);
        fat(&mut disk, 0, 0x0fff_fff8);
        fat(&mut disk, 1, 0x0fff_ffff);
        let primary = *disk.blocks.get(&(START + 32)).unwrap();
        disk.blocks.insert(START + 32 + 513, primary);
        disk
    }
    fn mount() -> Fat32<Partition<Disk>> {
        Fat32::mount(Partition::mbr(disk(), 0).unwrap()).unwrap()
    }
    fn check(bytes: &[u8], offset: usize) {
        for (i, byte) in bytes.iter().enumerate() {
            assert_eq!(*byte, pattern(offset + i), "offset {}", offset + i);
        }
    }
    #[test]
    fn shared_cursor_keeps_completed_progress_on_io_error() {
        let volume = SharedVolume::new(Fat32::mount(Partition::mbr(disk(), 0).unwrap()).unwrap());
        let mut a = SharedFile::open(&volume, "FAST.BIN").unwrap();
        let mut b = SharedFile::open(&volume, "FRAG.BIN").unwrap();
        b.seek(4095).unwrap();
        volume.inner.borrow_mut().device.device_mut().fail_at = Some(cluster_lba(7));
        let mut out = vec![0; a.len() as usize];
        assert!(a.read(&mut out).is_err());
        assert_eq!(a.position(), 8192);
        assert_eq!(b.position(), 4095);
        volume.inner.borrow_mut().device.device_mut().fail_at = None;
        let pos = a.position() as usize;
        assert_eq!(a.read(&mut out[pos..]).unwrap(), out.len() - pos);
        assert_eq!(a.position(), a.len());
        assert_eq!(b.read(&mut [0; 3]).unwrap(), 3);
        assert_eq!(b.position(), 4098);
    }
    #[test]
    fn shared_cursors_preserve_offsets_and_cluster_hints() {
        let mut fs = Fat32::mount(Partition::mbr(disk(), 0).unwrap()).unwrap();
        let mut a = fs.open("FAST.BIN").unwrap();
        let mut fast = vec![0; a.len() as usize];
        a.read(&mut fast).unwrap();
        let mut b = fs.open("FRAG.BIN").unwrap();
        let mut frag = vec![0; b.len() as usize];
        b.read(&mut frag).unwrap();
        let volume = SharedVolume::new(fs);
        let mut a = SharedFile::open(&volume, "FAST.BIN").unwrap();
        let mut b = SharedFile::open(&volume, "FRAG.BIN").unwrap();
        for (ap, bp) in [(0, 0), (4093, 4095), (8190, 8001), (31, 63), (12000, 8999)] {
            a.seek(ap).unwrap();
            b.seek(bp).unwrap();
            for _ in 0..3 {
                let mut buf = [0; 777];
                let p = a.position() as usize;
                let n = a.read(&mut buf).unwrap();
                assert_eq!(&buf[..n], &fast[p..p + n]);
                let p = b.position() as usize;
                let n = b.read(&mut buf).unwrap();
                assert_eq!(&buf[..n], &frag[p..p + n]);
            }
        }
    }
    #[test]
    fn directories_paths_and_empty_files() {
        let mut fs = mount();
        let mut entries = Vec::new();
        fs.read_dir("/", |entry| {
            entries.push(entry);
            true
        })
        .unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(std::format!("{}", entries[0].name), "FAST.BIN");
        let mut file = fs.open("//sub/./nested.bin").unwrap();
        assert_eq!(file.len(), 12165);
        let mut bytes = [0; 33];
        assert_eq!(file.read(&mut bytes).unwrap(), 33);
        check(&bytes, 0);
        let mut empty = fs.open("EMPTY.TXT").unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.read(&mut bytes).unwrap(), 0);
        assert!(matches!(fs.open("MISSING.TXT"), Err(Error::NotFound)));
        assert!(matches!(fs.open("SUB"), Err(Error::NotFile)));
        assert!(matches!(fs.open("../FAST.BIN"), Err(Error::Unsupported(_))));
        assert!(matches!(fs.open("FAST.BIN/"), Err(Error::NotDirectory)));
    }
    #[test]
    fn contiguous_data_batches_cross_clusters_without_sector_cache() {
        let mut fs = mount();
        let mut file = fs.open("FAST.BIN").unwrap();
        file.fs.device.device.requests.clear();
        let mut out = vec![0; 16384];
        let n = file.read(&mut out).unwrap();
        assert_eq!(n, 12165);
        check(&out[..n], 0);
        assert!(out[n..].iter().all(|b| *b == 0));
        assert!(file
            .fs
            .device
            .device
            .requests
            .contains(&(cluster_lba(5), 8192)));
        assert_eq!(file.read(&mut out).unwrap(), 0);
        file.seek(511).unwrap();
        assert_eq!(file.read(&mut out[..8192]).unwrap(), 8192);
        check(&out[..8192], 511);
        file.seek(0).unwrap();
        file.read(&mut out[..17]).unwrap();
        check(&out[..17], 0);
        assert!(matches!(file.seek(12166), Err(Error::InvalidOffset)));
    }
    #[test]
    fn fragmented_file_and_unaligned_seeks_match_contents() {
        let mut fs = mount();
        let mut file = fs.open("FRAG.BIN").unwrap();
        let mut out = vec![0; 9000];
        assert_eq!(file.read(&mut out).unwrap(), 9000);
        check(&out, 0);
        assert!(!file
            .fs
            .device
            .device
            .requests
            .iter()
            .any(|(lba, len)| *lba == cluster_lba(10) && *len > 4096));
        for (position, len) in [(4095, 3), (8190, 800), (2, 700), (4500, 4096)] {
            file.seek(position).unwrap();
            let n = file.read(&mut out[..len]).unwrap();
            check(&out[..n], position as usize);
        }
    }
    #[test]
    fn invalid_geometry_partition_and_short_chain_are_rejected() {
        let mut d = disk();
        put32(d.blocks.get_mut(&0).unwrap(), 458, TOTAL + 1);
        assert!(matches!(Partition::mbr(d, 0), Err(Error::Invalid(_))));
        let mut d = disk();
        put32(d.blocks.get_mut(&START).unwrap(), 36, 1);
        assert!(matches!(
            Fat32::mount(Partition::mbr(d, 0).unwrap()),
            Err(Error::Invalid(_))
        ));
        let mut d = disk();
        put16(d.blocks.get_mut(&START).unwrap(), 40, 0x82);
        assert!(matches!(
            Fat32::mount(Partition::mbr(d, 0).unwrap()),
            Err(Error::Invalid(_))
        ));
        let mut fs = mount();
        let mut f = fs.open("BROKEN.BIN").unwrap();
        assert!(matches!(f.read(&mut [0; 8192]), Err(Error::Invalid(_))));
    }
    #[test]
    fn failed_cache_read_does_not_publish_stale_data() {
        let mut fs = mount();
        let mut f = fs.open("FAST.BIN").unwrap();
        f.fs.device.device.fail_at = Some(cluster_lba(5));
        assert!(f.read(&mut [0; 100]).is_err());
        assert_eq!(f.position(), 0);
        let mut out = [0; 100];
        assert_eq!(f.read(&mut out).unwrap(), 100);
        check(&out, 0);
        f.fs.device.device.fail_at = Some(cluster_lba(5) + 1);
        f.seek(512).unwrap();
        assert!(f.read(&mut [0; 100]).is_err());
        assert_eq!(f.position(), 512);
        f.seek(0).unwrap();
        f.read(&mut out).unwrap();
        check(&out, 0);
    }
    #[test]
    fn active_second_fat_and_reserved_high_bits() {
        let mut d = disk();
        put16(d.blocks.get_mut(&START).unwrap(), 40, 0x81);
        let first = *d.blocks.get(&(START + 32)).unwrap();
        d.blocks.insert(START + 32 + 513, first);
        d.blocks.insert(START + 32, [0; 512]);
        let block = d.blocks.get_mut(&(START + 32 + 513)).unwrap();
        put32(block, 5 * 4, 0xf000_0006);
        let mut fs = Fat32::mount(Partition::mbr(d, 0).unwrap()).unwrap();
        let mut f = fs.open("FAST.BIN").unwrap();
        let mut out = [0; 8192];
        f.read(&mut out).unwrap();
        check(&out, 0);
    }
    #[test]
    fn directory_cycles_and_partition_overruns_fail_without_writes() {
        let mut d = disk();
        fat(&mut d, 2, 2);
        for offset in 0..SPC {
            d.blocks
                .insert(cluster_lba(2) + u64::from(offset), [0xe5; 512]);
        }
        let mut fs = Fat32::mount(Partition::mbr(d, 0).unwrap()).unwrap();
        assert!(matches!(fs.read_dir("/", |_| true), Err(Error::Invalid(_))));
        let mut p = Partition::mbr(disk(), 0).unwrap();
        assert!(p.read(u64::from(TOTAL) - 1, &mut [0; 1024]).is_err());
        assert!(p.read(u64::MAX, &mut [0; 512]).is_err());
        assert!(p.write(u64::from(TOTAL), &[0; 512]).is_err());
        assert!(p.read(0, &mut [0; 513]).is_err());
    }
    fn writable_disk() -> Disk {
        let mut disk = disk();
        disk.writable = true;
        let boot = disk.blocks.get_mut(&START).unwrap();
        put16(boot, 48, 1);
        put16(boot, 50, 6);
        let mut info = [0; 512];
        put32(&mut info, 0, 0x41615252);
        put32(&mut info, 484, 0x61417272);
        put32(&mut info, 488, 65_500);
        put32(&mut info, 492, 4);
        put32(&mut info, 508, 0xaa550000);
        disk.blocks.insert(START + 1, info);
        disk.blocks.insert(START + 7, info);
        disk
    }
    fn raw_fat(d: &Disk, copy: u64, cluster: u32) -> u32 {
        le32(
            d.blocks
                .get(&(START + 32 + copy * 513 + u64::from(cluster / 128)))
                .unwrap(),
            (cluster % 128) as usize * 4,
        )
    }
    #[test]
    fn stream_arbitrary_chunks_publish_at_finish_and_remount() {
        assert!(core::mem::size_of::<FileWriter<'_, Partition<Disk>>>() < 768);
        let mut fs = Fat32::mount(Partition::mbr(writable_disk(), 0).unwrap()).unwrap();
        let data: Vec<u8> = (0..91_123).map(pattern).collect();
        let mut writer = fs.create_stream("STREAM.BIN").unwrap();
        writer.write(&[]).unwrap();
        let sizes = [1, 511, 3, 513, 8192, 17, 4096];
        let mut done = 0;
        let mut step = 0;
        while done < data.len() {
            let n = sizes[step % sizes.len()].min(data.len() - done);
            writer.write(&data[done..done + n]).unwrap();
            done += n;
            step += 1;
            assert_eq!(writer.len() as usize, done);
            assert!(matches!(
                writer.fs.resolve("STREAM.BIN"),
                Err(Error::NotFound)
            ));
            assert_eq!(raw_fat(&writer.fs.device.device, 0, 1) & (1 << 27), 0);
        }
        writer.finish().unwrap();
        assert!(fs
            .device
            .device
            .writes
            .iter()
            .any(|(_, bytes)| *bytes >= 4096));
        let mut fs = Fat32::mount(fs.into_inner()).unwrap();
        let mut actual = vec![0; data.len() + 1];
        let mut file = fs.open("STREAM.BIN").unwrap();
        assert_eq!(file.read(&mut actual).unwrap(), data.len());
        assert_eq!(&actual[..data.len()], &data);
        let mut old = fs.open("FAST.BIN").unwrap();
        let mut bytes = [0; 1024];
        old.read(&mut bytes).unwrap();
        check(&bytes, 0);
    }
    #[test]
    fn stream_abort_frees_chain_and_keeps_volume_writable() {
        let mut fs = Fat32::mount(Partition::mbr(writable_disk(), 0).unwrap()).unwrap();
        let mut writer = fs.create_stream("CANCEL.BIN").unwrap();
        writer.write(&vec![0x73; 9001]).unwrap();
        let mut allocated = Vec::new();
        let mut cluster = Some(writer.first);
        while let Some(current) = cluster {
            allocated.push(current);
            cluster = writer.fs.next_cluster(current).unwrap();
        }
        assert_eq!(allocated.len(), 3);
        writer.abort().unwrap();
        assert!(matches!(fs.open("CANCEL.BIN"), Err(Error::NotFound)));
        for copy in 0..2 {
            for &cluster in &allocated {
                assert_eq!(raw_fat(&fs.device.device, copy, cluster) & 0x0fff_ffff, 0);
            }
            assert_ne!(raw_fat(&fs.device.device, copy, 1) & (1 << 27), 0);
        }
        fs.create_stream("VOID.TXT").unwrap().abort().unwrap();
        fs.create_new("AFTER.TXT", b"healthy").unwrap();
    }
    #[test]
    fn abandoned_or_failed_stream_does_not_publish_and_requires_recovery() {
        let mut fs = Fat32::mount(Partition::mbr(writable_disk(), 0).unwrap()).unwrap();
        {
            let mut writer = fs.create_stream("DROP.BIN").unwrap();
            writer.write(b"pending edge").unwrap();
        }
        assert!(matches!(fs.open("DROP.BIN"), Err(Error::NotFound)));
        assert!(matches!(
            fs.create_stream("NEW.TXT"),
            Err(Error::NeedsRecovery)
        ));
        let mut fs = Fat32::mount(fs.into_inner()).unwrap();
        assert!(matches!(
            fs.create_stream("NEW.TXT"),
            Err(Error::NeedsRecovery)
        ));

        let mut disk = writable_disk();
        disk.fail_write = Some(cluster_lba(4) + 1);
        let mut fs = Fat32::mount(Partition::mbr(disk, 0).unwrap()).unwrap();
        let mut writer = fs.create_stream("FAIL.BIN").unwrap();
        writer.write(&[3; 512]).unwrap();
        assert!(writer.write(&[4; 512]).is_err());
        assert!(matches!(writer.write(&[5]), Err(Error::NeedsRecovery)));
        assert!(matches!(writer.abort(), Err(Error::NeedsRecovery)));
        assert!(matches!(fs.open("FAIL.BIN"), Err(Error::NotFound)));
    }
    #[test]
    fn stream_clean_flag_failure_between_fat_copies_blocks_new_writes() {
        let mut fs = Fat32::mount(Partition::mbr(writable_disk(), 0).unwrap()).unwrap();
        let mut writer = fs.create_stream("DONE.TXT").unwrap();
        writer.write(b"complete data").unwrap();
        writer.fs.device.device.fail_write = Some(START + 32 + 513);
        assert!(writer.finish().is_err());
        let mut fs = Fat32::mount(fs.into_inner()).unwrap();
        let mut out = [0; 13];
        assert_eq!(fs.open("DONE.TXT").unwrap().read(&mut out).unwrap(), 13);
        assert_eq!(&out, b"complete data");
        assert!(matches!(
            fs.create_stream("NEXT.TXT"),
            Err(Error::NeedsRecovery)
        ));
    }
    #[test]
    fn create_fragmented_file_remount_preserves_existing_and_both_fats() {
        let mut d = writable_disk();
        put32(d.blocks.get_mut(&(START + 32)).unwrap(), 4 * 4, 0xa0000000);
        put32(
            d.blocks.get_mut(&(START + 32 + 513)).unwrap(),
            4 * 4,
            0xb0000000,
        );
        let old_root = *d.blocks.get(&cluster_lba(2)).unwrap();
        let mut fs = Fat32::mount(Partition::mbr(d, 0).unwrap()).unwrap();
        let data: Vec<u8> = (0..9000).map(pattern).collect();
        fs.create_new("/CONFIG.TXT", &data).unwrap();
        let d = fs.into_inner().into_inner();
        assert_eq!(
            &d.blocks.get(&cluster_lba(2)).unwrap()[..160],
            &old_root[..160]
        );
        assert_eq!(raw_fat(&d, 0, 4), 0xa0000008);
        assert_eq!(raw_fat(&d, 1, 4), 0xb0000008);
        for copy in 0..2 {
            assert_eq!(raw_fat(&d, copy, 8) & 0x0fffffff, 9);
            assert_eq!(raw_fat(&d, copy, 9) & 0x0fffffff, 0x0fffffff);
            assert_eq!(raw_fat(&d, copy, 1) & (1 << 27), 1 << 27);
        }
        for sector in [1, 7] {
            let info = d.blocks.get(&(START + sector)).unwrap();
            assert_eq!(le32(info, 488), u32::MAX);
            assert_eq!(le32(info, 492), u32::MAX);
        }
        let mut fs = Fat32::mount(Partition::mbr(d, 0).unwrap()).unwrap();
        let mut file = fs.open("config.txt").unwrap();
        let mut actual = vec![0; 9001];
        assert_eq!(file.read(&mut actual).unwrap(), 9000);
        assert_eq!(&actual[..9000], &data);
        let mut old = fs.open("FAST.BIN").unwrap();
        let mut bytes = [0; 8192];
        old.read(&mut bytes).unwrap();
        check(&bytes, 0);
        let writes = fs.device.device.writes.len();
        assert!(matches!(
            fs.create_new("CONFIG.TXT", b"overwrite"),
            Err(Error::AlreadyExists)
        ));
        assert_eq!(fs.device.device.writes.len(), writes);
    }
    #[test]
    fn create_empty_and_nested_file_without_modifying_other_sectors() {
        let mut fs = Fat32::mount(Partition::mbr(writable_disk(), 0).unwrap()).unwrap();
        fs.create_new("ZERO.TXT", b"").unwrap();
        let mut empty = fs.open("ZERO.TXT").unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.read(&mut [0; 3]).unwrap(), 0);
        fs.create_new("SUB/BOARD.INI", b"brightness=25\n").unwrap();
        let mut f = fs.open("SUB/BOARD.INI").unwrap();
        let mut out = [0; 100];
        assert_eq!(f.read(&mut out).unwrap(), 14);
        assert_eq!(&out[..14], b"brightness=25\n");
    }
    #[test]
    fn failed_data_write_keeps_file_unpublished_and_requires_recovery() {
        let mut d = writable_disk();
        d.fail_write = Some(cluster_lba(4));
        let old_root = *d.blocks.get(&cluster_lba(2)).unwrap();
        let mut fs = Fat32::mount(Partition::mbr(d, 0).unwrap()).unwrap();
        assert!(fs.create_new("FAIL.TXT", b"test").is_err());
        assert!(matches!(fs.open("FAIL.TXT"), Err(Error::NotFound)));
        assert_eq!(
            *fs.device.device.blocks.get(&cluster_lba(2)).unwrap(),
            old_root
        );
        assert_eq!(raw_fat(&fs.device.device, 0, 1) & (1 << 27), 0);
        assert!(matches!(
            fs.create_new("AGAIN.TXT", b"test"),
            Err(Error::NeedsRecovery)
        ));
        let mut fs = Fat32::mount(fs.into_inner()).unwrap();
        assert!(matches!(
            fs.create_new("AGAIN.TXT", b"test"),
            Err(Error::NeedsRecovery)
        ));
    }
    #[test]
    fn create_uses_only_active_fat_and_preserves_end_marker() {
        let mut d = writable_disk();
        put16(d.blocks.get_mut(&START).unwrap(), 40, 0x81);
        let original_primary = *d.blocks.get(&(START + 32)).unwrap();
        // End marker at the last entry in a sector; following sector contains garbage.
        let mut root = [b'V'; 512];
        for offset in (0..480).step_by(32) {
            root[offset + 11] = 0x08;
        } // Occupied volume-label records.
        root[480] = 0;
        d.blocks.insert(cluster_lba(2), root);
        d.blocks.insert(cluster_lba(2) + 1, [0x41; 512]);
        let mut fs = Fat32::mount(Partition::mbr(d, 0).unwrap()).unwrap();
        fs.create_new("CONFIG.TXT", b"a=1\n").unwrap();
        assert_eq!(
            *fs.device.device.blocks.get(&(START + 32)).unwrap(),
            original_primary
        );
        assert_eq!(
            fs.device.device.blocks.get(&(cluster_lba(2) + 1)).unwrap()[0],
            0
        );
        let mut entries = Vec::new();
        fs.read_dir("/", |e| {
            entries.push(e);
            true
        })
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(std::format!("{}", entries[0].name), "CONFIG.TXT");
    }
    #[test]
    fn dirty_mirror_blocks_creation_before_any_write() {
        let mut d = writable_disk();
        put32(
            d.blocks.get_mut(&(START + 32 + 513)).unwrap(),
            4,
            0x07ffffff,
        );
        let mut fs = Fat32::mount(Partition::mbr(d, 0).unwrap()).unwrap();
        assert!(matches!(
            fs.create_new("CONFIG.TXT", b"test"),
            Err(Error::NeedsRecovery)
        ));
        assert!(fs.device.device.writes.is_empty());
    }
}
