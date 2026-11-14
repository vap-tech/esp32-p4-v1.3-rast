//! Cached alarm PCM. SD is used only to populate the cache before WEB READY.
//! Playback never allocates and never opens another SD controller/filesystem.
use super::*;
use crate::background::Services;
use esp32_p4::{
    devices::es8311::Es8311,
    media::{pcm::Cursor, wav::Wave, Input},
    memory::{Buffer, External, Internal},
};
const GUARD: usize = 64;
const LIMIT: usize = 8 * 1024 * 1024;
struct Bytes<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Input for Bytes<'_> {
    type Error = &'static str;
    fn len(&self) -> u32 {
        self.bytes.len() as u32
    }
    fn position(&self) -> u32 {
        self.at as u32
    }
    fn seek(&mut self, at: u32) -> Result<(), Self::Error> {
        if at as usize > self.bytes.len() {
            return Err("WAV seek");
        }
        self.at = at as usize;
        Ok(())
    }
    fn read(&mut self, out: &mut [u8]) -> Result<usize, Self::Error> {
        let n = out.len().min(self.bytes.len() - self.at);
        out[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}
pub(super) struct Player<'a> {
    source: Buffer<'a, External>,
    frames: Option<Buffer<'a, Internal>>,
    wave: Wave,
    cursor: Cursor,
    tx: Option<hal::i2s::Tx>,
    codec: Es8311,
    pa: hal::gpio::Output<11>,
    index: usize,
    retry: u64,
    last_poll: u64,
    max_gap_us: u64,
    max_refill_us: u64,
    min_owned: u32,
    refills: u32,
    starts: u32,
    errors: u32,
    underruns: u32,
}
impl<'a> Player<'a> {
    fn refill(&mut self, index: usize) -> Result<(), &'static str> {
        let data = &self.source.as_slice()[GUARD + self.wave.data_offset as usize
            ..GUARD + (self.wave.data_offset + self.wave.data_bytes) as usize];
        // Allocation is 64-byte aligned, exactly one 512-frame CPU workspace.
        let frames = unsafe {
            &mut *self
                .frames
                .as_mut()
                .unwrap()
                .as_mut_slice()
                .as_mut_ptr()
                .cast::<[u32; 512]>()
        };
        self.cursor.fill(data, self.wave.channels, frames)?;
        self.tx
            .as_mut()
            .ok_or("no I2S")?
            .stream_refill_now(index, frames)
            .map_err(|_| "I2S refill")
    }
    fn start(
        &mut self,
        services: &mut dyn Services,
        timer: &mut hal::time::Timer,
    ) -> Result<(), &'static str> {
        self.pa.set_low();
        self.cursor.rewind();
        self.tx = Some(unsafe { hal::i2s::Tx::new_stream(timer) }.map_err(|_| "I2S start")?);
        for i in 0..hal::i2s::STREAM_BUFFERS {
            self.refill(i)?;
        }
        self.tx
            .as_mut()
            .unwrap()
            .start(timer)
            .map_err(|_| "I2S run")?;
        services
            .mute(&mut self.codec, timer, false)
            .map_err(|_| "codec unmute")?;
        self.index = 0;
        self.last_poll = timer.now_us();
        self.pa.set_high();
        self.starts += 1;
        Ok(())
    }
    pub fn stop(
        &mut self,
        services: &mut dyn Services,
        timer: &mut hal::time::Timer,
        console: &mut hal::usb_serial::Console,
    ) {
        self.pa.set_low();
        if let Some(mut tx) = self.tx.take() {
            let mute = services.mute(&mut self.codec, timer, true);
            let stop = tx.stop(timer);
            assert!(
                stop.is_ok() || tx.status().link & 8 != 0,
                "I2S DMA stop unconfirmed"
            );
            let _ = writeln!(console, "WEB ALARM STOP mute={:?} dma={:?}", mute, stop);
            if mute.is_err() || stop.is_err() {
                self.errors += 1;
            }
        }
        self.last_poll = 0;
    }
    pub fn poll(
        &mut self,
        services: &mut dyn Services,
        timer: &mut hal::time::Timer,
        console: &mut hal::usb_serial::Console,
    ) {
        let now = timer.now_us();
        if !services.alarm_active() {
            self.retry = 0;
            if self.tx.is_some() {
                self.stop(services, timer, console);
            }
            return;
        }
        if self.tx.is_none() {
            if now < self.retry {
                return;
            }
            match self.start(services, timer) {
                Ok(()) => {
                    let _ = writeln!(console, "WEB ALARM START cached PCM rate=16000 DAC=-21dB");
                }
                Err(error) => {
                    self.errors += 1;
                    let _ = writeln!(console, "WEB ALARM ERROR {}", error);
                    self.stop(services, timer, console);
                    self.retry = timer.now_us() + 5_000_000;
                }
            }
            return;
        }
        self.max_gap_us = self.max_gap_us.max(now.wrapping_sub(self.last_poll));
        self.last_poll = now;
        let mask = self.tx.as_ref().unwrap().stream_owned_mask();
        self.min_owned = self.min_owned.min(mask.count_ones());
        for _ in 0..hal::i2s::STREAM_BUFFERS {
            let ready = self.tx.as_mut().unwrap().stream_released(self.index);
            if matches!(ready, Ok(false)) {
                break;
            }
            let started = timer.now_us();
            if ready.is_err() || self.refill(self.index).is_err() {
                self.errors += 1;
                let status = self.tx.as_ref().unwrap().status();
                if mask == 0 && status.dma & 4 != 0 {
                    self.underruns += 1;
                }
                let _ = writeln!(console, "WEB ALARM DMA ERROR mask={:x} {:?}", mask, status);
                self.stop(services, timer, console);
                self.retry = timer.now_us() + 5_000_000;
                break;
            }
            self.max_refill_us = self.max_refill_us.max(timer.now_us().wrapping_sub(started));
            self.refills += 1;
            self.index = (self.index + 1) % hal::i2s::STREAM_BUFFERS;
        }
    }
    pub fn take_workspace(&mut self) -> Option<Buffer<'a, Internal>> {
        self.frames.take()
    }
    pub fn report(&self, console: &mut hal::usb_serial::Console) {
        let guard = self.source.as_slice()[..GUARD]
            .iter()
            .chain(self.source.as_slice()[self.source.len() - GUARD..].iter())
            .all(|b| *b == 0xa5);
        let _=writeln!(console,"WEB ALARM SUMMARY starts={} refills={} loops={} min_owned={} max_poll_gap_us={} max_refill_us={} errors={} underruns={} guards={}",self.starts,self.refills,self.cursor.loops,self.min_owned,self.max_gap_us,self.max_refill_us,self.errors,self.underruns,guard);
    }
}
impl Drop for Player<'_> {
    fn drop(&mut self) {
        self.pa.set_low();
        if let Some(tx) = self.tx.as_mut() {
            tx.request_stop();
        }
    }
}
pub(super) fn load<'a, P: Port, D: disk::BlockDevice, const N: usize>(
    fs: &mut FileTable<'_, '_, D, N>,
    network: &RefCell<Network<'a, '_, '_, P>>,
    external: &'a memory::Arena<'_, External>,
    working: &'a memory::Arena<'_, Internal>,
    scratch: &mut [u8],
) -> Result<Player<'a>, &'static str> {
    use esp32_p4::storage::files::OpenMode;
    let handle = esp32_p4::runtime::run(fs.open("/SATIE.WAV", OpenMode::Read), || {
        network.borrow_mut().io_tick()
    })
    .map_err(|_| "network")?
    .map_err(|_| "SATIE.WAV missing/busy")?;
    let result = (|| {
        let len = fs.info(handle).map_err(|_| "WAV info")?.len as usize;
        if len == 0 || len > LIMIT {
            return Err("WAV exceeds 8MiB cache limit");
        }
        let mut failed = false;
        let mut source = external
            .allocate_paced_with(
                core::alloc::Layout::from_size_align(len + 2 * GUARD, 64).unwrap(),
                || {
                    failed |= network.borrow_mut().io_tick().is_err();
                },
            )
            .map_err(|_| "PSRAM cache")?;
        if failed {
            return Err("network during cache initialization");
        }
        source.as_mut_slice()[..GUARD].fill(0xa5);
        source.as_mut_slice()[GUARD + len..].fill(0xa5);
        let mut at = 0;
        while at < len {
            let want = scratch.len().min(len - at);
            let n = esp32_p4::runtime::run(fs.read(handle, &mut scratch[..want]), || {
                network.borrow_mut().io_tick()
            })
            .map_err(|_| "network")?
            .map_err(|_| "WAV read")?;
            if n == 0 {
                return Err("WAV truncated");
            }
            web::upload::copy_paced(
                &mut source.as_mut_slice()[GUARD + at..GUARD + at + n],
                &scratch[..n],
            );
            at += n;
            network.borrow_mut().io_tick().map_err(|_| "network")?;
            let net = network.borrow();
            if net.stop || net.port.borrow_mut().now_us() >= net.deadline {
                return Err("cache load cancelled");
            }
        }
        let wave = Wave::parse(&mut Bytes {
            bytes: &source.as_slice()[GUARD..GUARD + len],
            at: 0,
        })
        .map_err(|_| "WAV format")?;
        if wave.sample_rate != 16000 {
            return Err("alarm requires PCM16/16000Hz");
        }
        let mut pa = unsafe { hal::gpio::Output::<11>::new_low() };
        pa.set_low();
        let mut net = network.borrow_mut();
        let services = net.services.ok_or("no board services")?;
        let mut ui = services.borrow_mut();
        let codec = net.port.borrow_mut().with_timer(|timer| {
            let mut tx = unsafe { hal::i2s::Tx::new(&[0], timer) }.map_err(|_| "I2S clock")?;
            let result = (|| {
                tx.start(timer).map_err(|_| "I2S silence")?;
                ui.codec(timer).map_err(|_| "codec init")
            })();
            let stop = tx.stop(timer).map_err(|_| "I2S initial stop");
            assert!(
                stop.is_ok() || tx.status().link & 8 != 0,
                "I2S initial stop unconfirmed"
            );
            match (result, stop) {
                (Ok(codec), Ok(())) => Ok(codec),
                (Err(e), _) | (_, Err(e)) => Err(e),
            }
        })?;
        let frames = match net.spare_frames.take() {
            Some(buffer) => buffer,
            None => working
                .allocate(core::alloc::Layout::from_size_align(2048, 64).unwrap())
                .map_err(|_| "PCM workspace")?,
        };
        let _ = writeln!(
            net.console,
            "WEB ALARM CACHE bytes={} PCM={} channels={} PSRAM; playback does not use SD",
            len, wave.data_bytes, wave.channels
        );
        Ok(Player {
            source,
            frames: Some(frames),
            wave,
            cursor: Cursor::default(),
            tx: None,
            codec,
            pa,
            index: 0,
            retry: 0,
            last_poll: 0,
            max_gap_us: 0,
            max_refill_us: 0,
            min_owned: 4,
            refills: 0,
            starts: 0,
            errors: 0,
            underruns: 0,
        })
    })();
    let close = esp32_p4::runtime::run(fs.close(handle), || network.borrow_mut().io_tick())
        .map_err(|_| "network")?
        .map_err(|_| "WAV close");
    match (result, close) {
        (Ok(player), Ok(())) => Ok(player),
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
}
