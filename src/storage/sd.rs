//! Adapter only: the SDMMC HAL has no dependency on a filesystem.
use super::BlockDevice;
use crate::hal::{
    sdmmc::{Error as SdError, Sdmmc, MAX_TRANSFER},
    time::Timer,
};

#[derive(Debug)]
pub enum Error {
    AddressOverflow,
    Card(SdError),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ReadStats {
    pub calls: u32,
    pub bytes: u64,
    pub largest: usize,
    pub io_us: u64,
}

pub struct SdDevice<'a> {
    card: Sdmmc,
    timer: &'a mut Timer,
    stats: ReadStats,
}
impl<'a> SdDevice<'a> {
    pub fn new(card: Sdmmc, timer: &'a mut Timer) -> Self {
        Self {
            card,
            timer,
            stats: ReadStats::default(),
        }
    }
    pub fn stats(&self) -> ReadStats {
        self.stats
    }
}
impl BlockDevice for SdDevice<'_> {
    type Error = Error;
    fn sectors(&self) -> u64 {
        self.card.info.sectors
    }
    fn max_transfer_bytes(&self) -> usize {
        MAX_TRANSFER
    }
    fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Error> {
        let lba = u32::try_from(lba).map_err(|_| Error::AddressOverflow)?;
        let start = self.timer.now_us();
        let result = self
            .card
            .read_sectors(lba, out, self.timer)
            .map_err(Error::Card);
        self.stats.io_us += self.timer.now_us().wrapping_sub(start) & ((1 << 48) - 1);
        if result.is_ok() {
            self.stats.calls += 1;
            self.stats.bytes += out.len() as u64;
            self.stats.largest = self.stats.largest.max(out.len());
        }
        result
    }
    fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), Error> {
        self.card
            .write_sectors(
                u32::try_from(lba).map_err(|_| Error::AddressOverflow)?,
                data,
                self.timer,
            )
            .map_err(Error::Card)
    }
}

impl super::fat32::File<'_, super::Partition<SdDevice<'_>>> {
    /// Temporarily reborrow the SD transport's timer for another polled device.
    /// The file cannot perform I/O inside this closure, so timer access stays exclusive.
    pub fn with_timer<R>(&mut self, operation: impl FnOnce(&mut Timer) -> R) -> R {
        operation(self.device_mut().device_mut().timer)
    }
}
