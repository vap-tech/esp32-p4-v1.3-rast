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
const FAT_LINES: usize = 4;
struct FatLine {
    lba: Option<u64>,
    dirty: bool,
    bytes: [u8; 512],
}
impl FatLine {
    const EMPTY: Self = Self {
        lba: None,
        dirty: false,
        bytes: [0; 512],
    };
}
/// Byte I/O with two write-through partial-sector lines and a separate bounded
/// FAT write-back cache. Only scoped regular-file writes admit dirty FAT lines.
pub struct Stream<D> {
    pub(super) device: D,
    position: u64,
    cache: [[u8; 512]; 2],
    cached: [Option<u64>; 2],
    victim: usize,
    fat_range: Option<(u64, u64)>, // bytes within this partition
    fat: [FatLine; FAT_LINES],
    fat_victim: usize,
    defer_fat: bool,
    fat_flush_incomplete: bool,
}
impl<D: BlockDevice> Stream<D> {
    pub fn new(device: D) -> Self {
        Self {
            device,
            position: 0,
            cache: [[0; 512]; 2],
            cached: [None; 2],
            victim: 0,
            fat_range: None,
            fat: [const { FatLine::EMPTY }; FAT_LINES],
            fat_victim: 0,
            defer_fat: false,
            fat_flush_incomplete: false,
        }
    }
    pub fn capacity(&self) -> u64 {
        self.device.sectors() * 512
    }
    /// Extract only after an explicit checkpoint; never silently discard dirty FAT.
    pub fn into_inner(self) -> D {
        assert!(
            !self.has_dirty_fat(),
            "flush FAT before extracting the device"
        );
        self.device
    }
    pub fn configure_fat_cache(&mut self, start: u64, end: u64) -> Result<(), Error<D::Error>> {
        if start >= end
            || !start.is_multiple_of(512)
            || !end.is_multiple_of(512)
            || end > self.capacity()
            || self.defer_fat
        {
            return Err(Error::Invalid);
        }
        let range = Some((start, end));
        if self.has_dirty_fat() && self.fat_range != range {
            return Err(Error::Invalid);
        }
        if !self.has_dirty_fat() {
            for line in &mut self.fat {
                line.lba = None;
            }
            self.fat_victim = 0;
        }
        self.fat_range = range;
        self.cached = [None; 2];
        Ok(())
    }
    /// Discard clean read/FAT lines only after the volume's explicit checkpoint.
    pub fn invalidate_clean_caches(&mut self) -> Result<(), Error<D::Error>> {
        if self.defer_fat || self.has_dirty_fat() || self.fat_flush_incomplete {
            return Err(Error::Invalid);
        }
        self.cached = [None; 2];
        self.victim = 0;
        for line in &mut self.fat {
            line.lba = None;
        }
        self.fat_victim = 0;
        Ok(())
    }
    pub fn fat_flush_failed(&self) -> bool {
        self.fat_flush_incomplete
    }
    pub fn has_dirty_fat(&self) -> bool {
        self.fat.iter().any(|line| line.dirty)
    }
    fn in_fat(&self) -> bool {
        self.fat_range
            .is_some_and(|(start, end)| self.position >= start && self.position < end)
    }
    fn before_fat(&self, want: usize) -> usize {
        match self.fat_range {
            Some((start, _)) if self.position < start => want.min((start - self.position) as usize),
            _ => want,
        }
    }
    /// Persist all dirty physical FAT sectors and wait for the device checkpoint.
    /// On error/cancellation no line loses its dirty flag, including sectors that
    /// were written before the failure. Retrying repeats a consistent cached epoch.
    pub async fn flush_fat(&mut self) -> Result<(), Error<D::Error>> {
        if !self.has_dirty_fat() {
            return Ok(());
        }
        let failed_before = self.fat_flush_incomplete;
        self.fat_flush_incomplete = true;
        for line in &self.fat {
            if line.dirty {
                self.device
                    .write(line.lba.unwrap(), &line.bytes)
                    .await
                    .map_err(Error::Device)?;
            }
        }
        self.device.sync().await.map_err(Error::Device)?;
        for line in &mut self.fat {
            line.dirty = false;
        }
        // A raw retry may persist the data, but must not silently unpoison an
        // already-failed mounted volume. A fresh transport/mount is required.
        self.fat_flush_incomplete = failed_before;
        Ok(())
    }
    async fn fat_line(&mut self, lba: u64) -> Result<usize, Error<D::Error>> {
        if let Some(i) = self.fat.iter().position(|line| line.lba == Some(lba)) {
            return Ok(i);
        }
        let i = self
            .fat
            .iter()
            .position(|line| line.lba.is_none())
            .unwrap_or(self.fat_victim);
        // Drain the complete dirty epoch before reusing a line, including mirrors.
        if self.fat[i].dirty {
            self.flush_fat().await?;
        }
        self.fat[i].lba = None;
        self.device
            .read(lba, &mut self.fat[i].bytes)
            .await
            .map_err(Error::Device)?;
        self.fat[i].lba = Some(lba);
        self.fat_victim = (i + 1) % FAT_LINES;
        Ok(i)
    }
    pub(super) fn defer_file_fat(stream: &core::cell::RefCell<Self>) -> FatWriteScope<'_, D> {
        let mut io = stream.borrow_mut();
        assert!(!io.defer_fat, "file writes must be serialized");
        io.defer_fat = true;
        FatWriteScope { stream }
    }
    async fn cache(&mut self, lba: u64) -> Result<usize, Error<D::Error>> {
        if let Some(i) = self.cached.iter().position(|entry| *entry == Some(lba)) {
            return Ok(i);
        }
        let i = self
            .cached
            .iter()
            .position(Option::is_none)
            .unwrap_or(self.victim);
        self.cached[i] = None;
        self.device
            .read(lba, &mut self.cache[i])
            .await
            .map_err(Error::Device)?;
        self.cached[i] = Some(lba);
        self.victim = (i + 1) % self.cached.len();
        Ok(i)
    }
    fn invalidate(&mut self, lba: u64, sectors: u64) {
        for entry in &mut self.cached {
            if entry.is_some_and(|n| n >= lba && n - lba < sectors) {
                *entry = None;
            }
        }
    }
}
pub(super) struct FatWriteScope<'a, D: BlockDevice> {
    stream: &'a core::cell::RefCell<Stream<D>>,
}
impl<D: BlockDevice> Drop for FatWriteScope<'_, D> {
    fn drop(&mut self) {
        self.stream.borrow_mut().defer_fat = false;
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
        let n = if self.in_fat() {
            let i = self.fat_line(self.position / 512).await?;
            let n = want.min(512 - offset);
            out[..n].copy_from_slice(&self.fat[i].bytes[offset..offset + n]);
            n
        } else if offset == 0 && want >= 512 {
            let n = self.before_fat(want).min(self.device.max_transfer_bytes()) / 512 * 512;
            self.device
                .read(self.position / 512, &mut out[..n])
                .await
                .map_err(Error::Device)?;
            n
        } else {
            let i = self.cache(self.position / 512).await?;
            let n = want.min(512 - offset);
            out[..n].copy_from_slice(&self.cache[i][offset..offset + n]);
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
        // Outside a regular-file data write, metadata remains write-through.
        // Drain FAT before publishing a directory/FSInfo/dirty-marker update.
        if !self.defer_fat {
            self.flush_fat().await?;
        }
        let n = if self.in_fat() {
            let i = self.fat_line(self.position / 512).await?;
            let n = want.min(512 - offset);
            self.fat[i].bytes[offset..offset + n].copy_from_slice(&b[..n]);
            self.fat[i].dirty = true;
            if !self.defer_fat {
                self.flush_fat().await?;
            }
            n
        } else if offset == 0 && want >= 512 {
            let n = self.before_fat(want).min(self.device.max_transfer_bytes()) / 512 * 512;
            // Invalidate before starting IO, including on error/cancellation.
            // Unrelated FAT lines remain useful across sequential data writes.
            self.invalidate(self.position / 512, (n / 512) as u64);
            self.device
                .write(self.position / 512, &b[..n])
                .await
                .map_err(Error::Device)?;
            n
        } else {
            let i = self.cache(self.position / 512).await?;
            let n = want.min(512 - offset);
            self.cache[i][offset..offset + n].copy_from_slice(&b[..n]);
            // Invalid until the complete sector write succeeds, including after cancellation.
            self.cached[i] = None;
            self.device
                .write(self.position / 512, &self.cache[i])
                .await
                .map_err(Error::Device)?;
            self.cached[i] = Some(self.position / 512);
            n
        };
        self.position += n as u64;
        Ok(n)
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.flush_fat().await?;
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

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    #[derive(Debug)]
    struct Fault;
    struct Disk {
        blocks: [[u8; 512]; 8],
        reads: usize,
        fail: bool,
        pending: bool,
        fail_sync: bool,
        writes: std::vec::Vec<u64>,
        syncs: usize,
    }
    impl BlockDevice for Disk {
        type Error = Fault;
        fn sectors(&self) -> u64 {
            8
        }
        fn max_transfer_bytes(&self) -> usize {
            4096
        }
        async fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Fault> {
            self.reads += 1;
            for (i, b) in out.chunks_mut(512).enumerate() {
                b.copy_from_slice(&self.blocks[lba as usize + i]);
            }
            Ok(())
        }
        async fn write(&mut self, lba: u64, bytes: &[u8]) -> Result<(), Fault> {
            self.writes.push(lba);
            // A failed or cancelled write may already have changed the device.
            for (i, b) in bytes.chunks(512).enumerate() {
                self.blocks[lba as usize + i].copy_from_slice(b);
            }
            if self.fail {
                return Err(Fault);
            }
            if self.pending {
                core::future::pending::<()>().await;
            }
            Ok(())
        }
        async fn sync(&mut self) -> Result<(), Fault> {
            self.syncs += 1;
            if self.fail_sync {
                Err(Fault)
            } else {
                Ok(())
            }
        }
    }
    fn stream() -> Stream<Disk> {
        Stream::new(Disk {
            blocks: [[0; 512]; 8],
            reads: 0,
            fail: false,
            pending: false,
            fail_sync: false,
            writes: std::vec::Vec::new(),
            syncs: 0,
        })
    }
    fn run<T>(future: impl Future<Output = T>) -> T {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("unexpected pending"),
        }
    }
    #[test]
    fn fat_mirrors_survive_unrelated_data_and_overlaps_invalidate() {
        let mut s = stream();
        for lba in [0, 1] {
            run(s.seek(SeekFrom::Start(lba * 512))).unwrap();
            run(s.write(&[7, 8, 9, 10])).unwrap();
        }
        assert_eq!(s.device.reads, 2);
        run(s.seek(SeekFrom::Start(4 * 512))).unwrap();
        run(s.write(&[33; 1024])).unwrap();
        for lba in [0, 1] {
            run(s.seek(SeekFrom::Start(lba * 512))).unwrap();
            run(s.write(&[11, 12, 13, 14])).unwrap();
            assert_eq!(&s.device.blocks[lba as usize][..4], &[11, 12, 13, 14]);
        }
        assert_eq!(s.device.reads, 2);
        run(s.seek(SeekFrom::Start(0))).unwrap();
        run(s.write(&[55; 1024])).unwrap();
        for lba in [0, 1] {
            run(s.seek(SeekFrom::Start(lba * 512))).unwrap();
            let mut b = [0; 4];
            run(s.read(&mut b)).unwrap();
            assert_eq!(b, [55; 4]);
        }
        assert_eq!(s.device.reads, 4);
    }
    #[test]
    fn partial_write_failure_and_cancellation_never_publish_cache() {
        for pending in [false, true] {
            let mut s = stream();
            run(s.write(&[1, 2, 3, 4])).unwrap();
            run(s.seek(SeekFrom::Start(0))).unwrap();
            s.device.fail = !pending;
            s.device.pending = pending;
            {
                let mut future = pin!(s.write(&[9, 8, 7, 6]));
                let result = future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()));
                if pending {
                    assert!(result.is_pending());
                } else {
                    assert!(matches!(result, Poll::Ready(Err(_))));
                }
            }
            assert_eq!(s.cached, [None, None]);
            let mut b = [0; 4];
            run(s.read(&mut b)).unwrap();
            assert_eq!(b, [9, 8, 7, 6]);
            assert_eq!(s.device.reads, 2);
        }
    }
    fn write_at(s: &mut Stream<Disk>, position: u64, bytes: &[u8]) {
        run(s.seek(SeekFrom::Start(position))).unwrap();
        run(s.write_all(bytes)).unwrap();
    }
    fn cached_fat() -> Stream<Disk> {
        let mut s = stream();
        s.configure_fat_cache(512, 5 * 512).unwrap();
        s.defer_fat = true;
        s
    }
    #[test]
    fn fat_updates_coalesce_and_whole_sector_reads_see_dirty_data() {
        let mut s = cached_fat();
        for i in 0..32 {
            for lba in [1, 3] {
                write_at(&mut s, lba * 512 + i * 4, &(i as u32 + 7).to_le_bytes());
            }
        }
        assert!(s.device.writes.is_empty());
        run(s.seek(SeekFrom::Start(512))).unwrap();
        let mut sector = [0; 512];
        run(s.read_exact(&mut sector)).unwrap();
        for i in 0..32 {
            assert_eq!(&sector[i * 4..i * 4 + 4], &(i as u32 + 7).to_le_bytes());
        }
        write_at(&mut s, 6 * 512, &[0x7a; 512]);
        assert_eq!(s.device.writes, [6]); // file payload does not force FAT flush
        s.defer_fat = false;
        write_at(&mut s, 7 * 512, &[0x19; 512]); // metadata publication barrier
        assert_eq!(s.device.writes, [6, 1, 3, 7]);
        assert!(s.device.syncs > 0);
        assert!(!s.has_dirty_fat());
        assert_eq!(s.device.blocks[1], s.device.blocks[3]);
    }
    #[test]
    fn aligned_reads_crossing_fat_boundary_do_not_bypass_dirty_cache() {
        let mut s = cached_fat();
        write_at(&mut s, 512, &[0x8a; 512]);
        run(s.seek(SeekFrom::Start(0))).unwrap();
        let mut out = [0; 1024];
        run(s.read_exact(&mut out)).unwrap();
        assert_eq!(&out[..512], &[0; 512]);
        assert_eq!(&out[512..], &[0x8a; 512]);
        assert!(s.device.writes.is_empty());
    }
    #[test]
    fn failed_and_cancelled_fat_flush_keeps_dirty_epoch_for_retry() {
        for kind in 0..3 {
            let mut s = cached_fat();
            write_at(&mut s, 512, &[1; 512]);
            write_at(&mut s, 3 * 512, &[2; 512]);
            s.device.fail = kind == 0;
            s.device.pending = kind == 1;
            s.device.fail_sync = kind == 2;
            if kind == 1 {
                let mut future = pin!(s.flush_fat());
                assert!(future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending());
            } else {
                assert!(run(s.flush_fat()).is_err());
            }
            assert!(s.fat_flush_failed());
            assert_eq!(s.fat.iter().filter(|line| line.dirty).count(), 2);
            s.device.fail = false;
            s.device.pending = false;
            s.device.fail_sync = false;
            run(s.flush_fat()).unwrap();
            assert!(!s.has_dirty_fat());
            assert!(s.fat_flush_failed()); // retry cannot resume a poisoned volume
            assert_eq!(s.device.blocks[1], [1; 512]);
            assert_eq!(s.device.blocks[3], [2; 512]);
        }
    }
    #[test]
    fn dirty_eviction_flushes_all_copies_and_cancelled_scope_restores_policy() {
        let stream = core::cell::RefCell::new(stream());
        stream
            .borrow_mut()
            .configure_fat_cache(512, 7 * 512)
            .unwrap();
        {
            let _lease = Stream::defer_file_fat(&stream);
            for lba in 1..=4 {
                write_at(&mut stream.borrow_mut(), lba * 512, &[lba as u8; 512]);
            }
            assert!(stream.borrow().device.writes.is_empty());
            // A fifth physical FAT sector forces eviction of a complete epoch.
            write_at(&mut stream.borrow_mut(), 5 * 512, &[5; 512]);
            assert_eq!(stream.borrow().device.writes, [1, 2, 3, 4]);
        }
        assert!(!stream.borrow().defer_fat);
        stream.borrow_mut().device.pending = true;
        {
            let mut future = pin!(async {
                let _lease = Stream::defer_file_fat(&stream);
                write_async(&stream).await
            });
            assert!(future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending());
        }
        assert!(!stream.borrow().defer_fat);
        assert!(stream.borrow().has_dirty_fat());
    }
    async fn write_async(stream: &core::cell::RefCell<Stream<Disk>>) -> Result<(), Error<Fault>> {
        let mut io = stream.borrow_mut();
        io.seek(SeekFrom::Start(7 * 512)).await?;
        io.write_all(&[9; 512]).await
    }
}
