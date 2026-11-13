//! Filesystems over bounded sector I/O. No allocator, hardware or runtime required.
pub mod asynchronous;
pub mod fat32;
pub mod format;
pub mod sd;
pub mod volume;

pub const SECTOR_SIZE: usize = 512;

/// Sector I/O: slices must contain whole 512-byte sectors. Implementations must
/// reject out-of-range requests; max_transfer_bytes is a nonzero multiple of 512.
pub trait BlockDevice {
    type Error;
    fn sectors(&self) -> u64;
    fn max_transfer_bytes(&self) -> usize;
    fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Self::Error>;
    fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum Error<E> {
    Device(E),
    Invalid(&'static str),
    Unsupported(&'static str),
    NotFound,
    NotFile,
    NotDirectory,
    InvalidName,
    InvalidOffset,
    AlreadyExists,
    NoSpace,
    DirectoryFull,
    DirectoryNotEmpty,
    NeedsRecovery,
}

/// A bounded view of a partition; filesystem sector zero is its boot sector.
pub struct Partition<D> {
    device: D,
    start: u64,
    sectors: u64,
}
impl<D: BlockDevice> Partition<D> {
    pub fn new(device: D, start: u64, sectors: u64) -> Result<Self, Error<D::Error>> {
        if sectors == 0
            || start
                .checked_add(sectors)
                .is_none_or(|end| end > device.sectors())
        {
            return Err(Error::Invalid("partition bounds"));
        }
        Ok(Self {
            device,
            start,
            sectors,
        })
    }
    /// Read one of four primary MBR partitions. GPT/extended partitions are not supported.
    pub fn mbr(mut device: D, index: usize) -> Result<Self, Error<D::Error>> {
        if index >= 4 {
            return Err(Error::Invalid("partition index"));
        }
        let mut boot = [0; 512];
        device.read(0, &mut boot).map_err(Error::Device)?;
        if boot[510..] != [0x55, 0xaa] {
            return Err(Error::Invalid("MBR signature"));
        }
        let entry = &boot[446 + index * 16..462 + index * 16];
        if entry[0] & 0x7f != 0 {
            return Err(Error::Invalid("partition status"));
        }
        if !matches!(entry[4], 0x0b | 0x0c) {
            return Err(Error::Unsupported("partition type (expected FAT32)"));
        }
        let start = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as u64;
        let sectors = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as u64;
        if start == 0 {
            return Err(Error::Invalid("partition overlaps MBR"));
        }
        Self::new(device, start, sectors)
    }
    pub(crate) fn device_mut(&mut self) -> &mut D {
        &mut self.device
    }
    pub fn start(&self) -> u64 {
        self.start
    }
    pub fn into_inner(self) -> D {
        self.device
    }
    fn physical(&self, lba: u64, bytes: usize) -> Result<u64, Error<D::Error>> {
        if bytes == 0
            || !bytes.is_multiple_of(512)
            || bytes > self.device.max_transfer_bytes()
            || lba
                .checked_add((bytes / 512) as u64)
                .is_none_or(|end| end > self.sectors)
        {
            return Err(Error::Invalid("partition I/O bounds"));
        }
        Ok(self.start + lba)
    }
}
impl<D: BlockDevice> BlockDevice for Partition<D> {
    type Error = Error<D::Error>;
    fn sectors(&self) -> u64 {
        self.sectors
    }
    fn max_transfer_bytes(&self) -> usize {
        self.device.max_transfer_bytes()
    }
    fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Self::Error> {
        let physical = self.physical(lba, out.len())?;
        self.device.read(physical, out).map_err(Error::Device)
    }
    fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), Self::Error> {
        let physical = self.physical(lba, data.len())?;
        self.device.write(physical, data).map_err(Error::Device)
    }
}
