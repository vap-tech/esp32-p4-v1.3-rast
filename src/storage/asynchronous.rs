//! Async sector boundary, independent of SDMMC, FAT and the executor.
// Filesystem access is serialized by the volume API; this borrow is the IO lease.
#![allow(clippy::await_holding_refcell_ref)]
use embedded_io_async::{ErrorKind, ErrorType, Read, Seek, SeekFrom, Write};
#[allow(async_fn_in_trait)]
pub trait BlockDevice {
    type Error: core::fmt::Debug;
    fn sectors(&self) -> u64;
    fn max_transfer_bytes(&self) -> usize;
    async fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Self::Error>;
    async fn write(&mut self, lba: u64, bytes: &[u8]) -> Result<(), Self::Error>;
    async fn sync(&mut self) -> Result<(), Self::Error>;
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E: core::fmt::Debug> {
    Device(E),
    Bounds,
    Invalid,
    Cancelled,
}
impl<E: core::fmt::Debug> core::fmt::Display for Error<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl<E: core::fmt::Debug> core::error::Error for Error<E> {}
impl<E: core::fmt::Debug> embedded_io_async::Error for Error<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Bounds | Self::Invalid => ErrorKind::InvalidInput,
            _ => ErrorKind::Other,
        }
    }
}
/// A partition is a bounded block-device view, not tied to FAT partition IDs.
pub struct Partition<D> {
    pub(super) device: D,
    pub(super) start: u64,
    pub(super) sectors: u64,
}
impl<D: BlockDevice> Partition<D> {
    pub fn start(&self) -> u64 {
        self.start
    }
    pub fn new(device: D, start: u64, sectors: u64) -> Result<Self, Error<D::Error>> {
        if sectors == 0
            || start
                .checked_add(sectors)
                .is_none_or(|end| end > device.sectors())
        {
            return Err(Error::Bounds);
        }
        Ok(Self {
            device,
            start,
            sectors,
        })
    }
    pub async fn mbr(mut device: D, index: usize) -> Result<Self, Error<D::Error>> {
        if index >= 4 {
            return Err(Error::Invalid);
        }
        let mut sector = [0; 512];
        device.read(0, &mut sector).await.map_err(Error::Device)?;
        if sector[510..] != [0x55, 0xaa] {
            return Err(Error::Invalid);
        }
        let e = &sector[446 + index * 16..462 + index * 16];
        if e[0] & 0x7f != 0 || e[4] == 0 || matches!(e[4], 0x05 | 0x0f | 0xee) {
            return Err(Error::Invalid);
        }
        let start = u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64;
        let sectors = u32::from_le_bytes(e[12..16].try_into().unwrap()) as u64;
        if start == 0 {
            return Err(Error::Invalid);
        }
        Self::new(device, start, sectors)
    }
    fn address(&self, lba: u64, len: usize) -> Result<u64, Error<D::Error>> {
        if len == 0
            || !len.is_multiple_of(512)
            || len > self.max_transfer_bytes()
            || lba
                .checked_add((len / 512) as u64)
                .is_none_or(|n| n > self.sectors)
        {
            return Err(Error::Bounds);
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
    async fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Self::Error> {
        let p = self.address(lba, out.len())?;
        self.device.read(p, out).await.map_err(Error::Device)
    }
    async fn write(&mut self, lba: u64, b: &[u8]) -> Result<(), Self::Error> {
        let p = self.address(lba, b.len())?;
        self.device.write(p, b).await.map_err(Error::Device)
    }
    async fn sync(&mut self) -> Result<(), Self::Error> {
        self.device.sync().await.map_err(Error::Device)
    }
}
/// Byte I/O required by FAT. Partial sectors use one write-through cache;
/// whole aligned runs go straight to the block device in bounded batches.
/// No dirty sector is silently dropped when seeking or cancelling a future.
pub struct Stream<D> {
    pub(super) device: D,
    position: u64,
    cache: [u8; 512],
    cached: Option<u64>,
}
impl<D: BlockDevice> Stream<D> {
    pub fn new(device: D) -> Self {
        Self {
            device,
            position: 0,
            cache: [0; 512],
            cached: None,
        }
    }
    pub fn capacity(&self) -> u64 {
        self.device.sectors() * 512
    }
    pub fn into_inner(self) -> D {
        self.device
    }
    async fn cache(&mut self, lba: u64) -> Result<(), Error<D::Error>> {
        if self.cached != Some(lba) {
            self.cached = None;
            self.device
                .read(lba, &mut self.cache)
                .await
                .map_err(Error::Device)?;
            self.cached = Some(lba);
        }
        Ok(())
    }
}
impl<D: BlockDevice> ErrorType for Stream<D> {
    type Error = Error<D::Error>;
}
impl<D: BlockDevice> Seek for Stream<D> {
    async fn seek(&mut self, from: SeekFrom) -> Result<u64, Self::Error> {
        let n = match from {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(n) => self.position.checked_add_signed(n),
            SeekFrom::End(n) => self.capacity().checked_add_signed(n),
        }
        .filter(|n| *n <= self.capacity())
        .ok_or(Error::Bounds)?;
        self.position = n;
        Ok(n)
    }
}
impl<D: BlockDevice> Read for Stream<D> {
    async fn read(&mut self, out: &mut [u8]) -> Result<usize, Self::Error> {
        let want = ((self.capacity() - self.position).min(out.len() as u64)) as usize;
        if want == 0 {
            return Ok(0);
        }
        let offset = self.position as usize % 512;
        let n = if offset == 0 && want >= 512 {
            let n = want.min(self.device.max_transfer_bytes()) / 512 * 512;
            self.device
                .read(self.position / 512, &mut out[..n])
                .await
                .map_err(Error::Device)?;
            n
        } else {
            self.cache(self.position / 512).await?;
            let n = want.min(512 - offset);
            out[..n].copy_from_slice(&self.cache[offset..offset + n]);
            n
        };
        self.position += n as u64;
        Ok(n)
    }
}
impl<D: BlockDevice> Write for Stream<D> {
    async fn write(&mut self, b: &[u8]) -> Result<usize, Self::Error> {
        if b.is_empty() {
            return Ok(0);
        }
        if self.position == self.capacity() {
            return Err(Error::Bounds);
        }
        let want = ((self.capacity() - self.position).min(b.len() as u64)) as usize;
        let offset = self.position as usize % 512;
        let n = if offset == 0 && want >= 512 {
            let n = want.min(self.device.max_transfer_bytes()) / 512 * 512;
            self.cached = None;
            self.device
                .write(self.position / 512, &b[..n])
                .await
                .map_err(Error::Device)?;
            n
        } else {
            self.cache(self.position / 512).await?;
            let n = want.min(512 - offset);
            self.cache[offset..offset + n].copy_from_slice(&b[..n]);
            // Invalid until the complete sector write succeeds, including after cancellation.
            self.cached = None;
            self.device
                .write(self.position / 512, &self.cache)
                .await
                .map_err(Error::Device)?;
            self.cached = Some(self.position / 512);
            n
        };
        self.position += n as u64;
        Ok(n)
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.device.sync().await.map_err(Error::Device)
    }
}
/// Short shared reborrows; the volume API serializes filesystem operations.
pub struct Shared<'a, D>(pub &'a core::cell::RefCell<Stream<D>>);
impl<D: BlockDevice> ErrorType for Shared<'_, D> {
    type Error = Error<D::Error>;
}
impl<D: BlockDevice> Read for Shared<'_, D> {
    async fn read(&mut self, b: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().read(b).await
    }
}
impl<D: BlockDevice> Write for Shared<'_, D> {
    async fn write(&mut self, b: &[u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().write(b).await
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().flush().await
    }
}
impl<D: BlockDevice> Seek for Shared<'_, D> {
    async fn seek(&mut self, p: SeekFrom) -> Result<u64, Self::Error> {
        self.0.borrow_mut().seek(p).await
    }
}
