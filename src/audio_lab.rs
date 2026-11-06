//! Long live-WAV/JPEG test. Two readonly cursors, one serialized FAT32/SD owner.
use super::hal;
use core::alloc::Layout;
use core::fmt::Write;
use esp32_p4::{
    devices::{es8311::Es8311, framebuffer::Framebuffer},
    media::{idct::Kind, jpeg, wav::Wave},
    memory::{Arena, Internal},
    storage::{
        fat32::{Fat32, SharedFile, SharedVolume},
        sd::SdDevice,
        Partition,
    },
};
use hal::i2s::{
    capture::{self, Ring, WORDS},
    Tx, STREAM_BUFFERS,
};
type Source<'a, 't> = SharedFile<'a, Partition<SdDevice<'t>>>;
const HISTORY: usize = 8192;
const RX_OFFSET: usize = 4096 + HISTORY * 4 + 64;
const RX_BYTES: usize = capture::BLOCKS * WORDS * 4;
const DESC_OFFSET: usize = RX_OFFSET + RX_BYTES + 64;
const TOTAL: usize = DESC_OFFSET + capture::BLOCKS * 64 + 64;

struct Live<'a, 't, 'm> {
    tx: Tx,
    rx: Option<Ring>,
    pa: hal::gpio::Output<11>,
    source: Source<'a, 't>,
    wave: Wave,
    frames: &'m mut [u32; 512],
    raw: &'m mut [u8; 2048],
    history: &'m mut [u32],
    running: bool,
    inject_repeat: bool,
    repeat_at: Option<u32>,
    produced: u32,
    index: usize,
    refills: u32,
    last_owned: [u64; STREAM_BUFFERS],
    first_free: [u64; STREAM_BUFFERS],
    lower: u64,
    upper: u64,
    min_owned: u32,
    underruns: u32,
    tx_errors: u32,
    last_poll: u64,
    max_gap: u64,
    max_fill: u64,
    aligned: Option<u32>,
    checked: u32,
    captured: u32,
    prefix: u32,
    mismatch: Option<(u32, u32, u32)>,
    failed: Option<&'static str>,
}
impl Live<'_, '_, '_> {
    fn fail(&mut self, reason: &'static str) {
        if self.failed.is_none() {
            if reason == "tx" {
                self.tx_errors += 1;
                // The injected underrun is classified only with an exhausted
                // ring and descriptor error; other TX errors stay separate.
                if self.tx.stream_owned_mask() == 0 && self.tx.status().dma & 4 != 0 {
                    self.underruns += 1;
                }
            }
            self.failed = Some(reason);
            self.pa.set_low();
            self.tx.request_stop();
        }
    }
    fn observe(&mut self) {
        let before = hal::time::cycle_count();
        let mask = self.tx.stream_owned_mask();
        let after = hal::time::cycle_count();
        self.min_owned = self.min_owned.min(mask.count_ones());
        for i in 0..STREAM_BUFFERS {
            if mask & (1 << i) != 0 {
                self.last_owned[i] = before;
            } else if self.first_free[i] == 0 {
                self.first_free[i] = after;
            }
        }
    }
    fn fill(&mut self, index: usize) -> bool {
        let started = hal::time::cycle_count();
        let n = match self
            .wave
            .read_frames_with_scratch(&mut self.source, self.frames, self.raw)
        {
            Ok(n) if n == 512 => n,
            _ => {
                self.fail("WAV read/EOF");
                return false;
            }
        };
        for i in 0..n {
            self.history[(self.produced as usize + i) % HISTORY] = self.frames[i];
        }
        // Reference history stays pristine. Deliberately repeat one TX frame
        // only after sequence alignment, to validate that RX checking catches it.
        if self.inject_repeat && self.repeat_at.is_none() && self.checked > 16_000 {
            if let Some(i) = (2..512).find(|i| self.frames[*i] != self.frames[*i - 1]) {
                self.frames[i] = self.frames[i - 1];
                self.repeat_at = Some(self.produced + i as u32);
            }
        }
        if self.running {
            self.observe();
        }
        if self.tx.stream_refill_now(index, self.frames).is_err() {
            self.fail("publish");
            return false;
        }
        let published = hal::time::cycle_count();
        self.max_fill = self.max_fill.max(published - started);
        if self.first_free[index] != 0 {
            self.lower = self.lower.max(published - self.first_free[index]);
            self.upper = self.upper.max(published - self.last_owned[index]);
        }
        self.last_owned[index] = published;
        self.first_free[index] = 0;
        self.produced += n as u32;
        self.refills += 1;
        true
    }
    fn verify_block(&mut self) {
        let rx = self.rx.as_ref().unwrap();
        let skip = usize::from(self.captured == 0); // RX startup word is not a complete stereo frame.
        if self.aligned.is_none() {
            let fingerprint: [u32; 64] = core::array::from_fn(|i| unsafe { rx.word(skip + i) });
            let first = self.produced.saturating_sub(HISTORY as u32);
            let mut candidate = None;
            let mut ambiguous = false;
            for at in first..self.produced.saturating_sub(64) {
                if (0..64).all(|i| self.history[(at as usize + i) % HISTORY] == fingerprint[i]) {
                    if candidate.is_some() {
                        ambiguous = true;
                        break;
                    }
                    candidate = Some(at);
                }
            }
            if !ambiguous {
                self.aligned = candidate;
            }
            if self.aligned.is_none() {
                self.prefix += WORDS as u32;
                self.captured += WORDS as u32;
                return;
            }
            self.prefix += skip as u32;
        }
        let mut expected = self.aligned.unwrap();
        for i in skip..WORDS {
            if expected < self.produced.saturating_sub(HISTORY as u32) || expected >= self.produced
            {
                self.fail("history coverage");
                return;
            }
            let actual = unsafe { self.rx.as_ref().unwrap().word(i) };
            let value = self.history[expected as usize % HISTORY];
            if actual != value {
                self.mismatch = Some((expected, value, actual));
                self.fail("sample sequence");
                return;
            }
            self.checked += 1;
            expected += 1;
        }
        self.aligned = Some(expected);
        self.captured += WORDS as u32;
    }
    fn pump(&mut self) {
        if self.failed.is_some() {
            return;
        }
        let before = hal::time::cycle_count();
        self.max_gap = self.max_gap.max(before - self.last_poll);
        self.last_poll = before;
        self.observe();
        for _ in 0..STREAM_BUFFERS {
            match self.tx.stream_released(self.index) {
                Ok(false) => break,
                Ok(true) => {
                    if !self.fill(self.index) {
                        return;
                    }
                    self.index = (self.index + 1) % STREAM_BUFFERS;
                }
                Err(_) => {
                    self.fail("tx");
                    return;
                }
            }
        }
        if self.rx.is_none() {
            return;
        }
        for _ in 0..capture::BLOCKS {
            match self.rx.as_ref().unwrap().ready() {
                Ok(false) => break,
                Err(_) => {
                    self.fail("rx");
                    return;
                }
                Ok(true) => {
                    self.verify_block();
                    if self.failed.is_some() {
                        return;
                    }
                    unsafe {
                        self.rx.as_mut().unwrap().release();
                    }
                }
            }
        }
    }
}

#[inline(never)]
pub fn run(
    fb: &mut Framebuffer,
    arena: &Arena<'_, Internal>,
    bus: &mut hal::i2c::I2c,
    timer: &mut hal::time::Timer,
    console: &mut hal::usb_serial::Console,
    command: &str,
) -> Result<(), &'static str> {
    let stall = command == "wav-lab-stall";
    let capture_enabled = command != "wav-lab-control";
    let repeat = command == "wav-lab-repeat";
    let mut allocation = arena
        .allocate(Layout::from_size_align(TOTAL, 64).unwrap())
        .map_err(|_| "arena")?;
    let base = allocation.as_mut_slice().as_mut_ptr();
    for offset in [RX_OFFSET - 64, RX_OFFSET + RX_BYTES, TOTAL - 64] {
        unsafe {
            base.add(offset).write_bytes(0xa5, 64);
        }
    }
    unsafe { capture::publish_region(base as usize, TOTAL, timer) }.map_err(|_| "flush")?;
    let mut card = unsafe { hal::sdmmc::Sdmmc::new(timer) }.map_err(|_| "SD init")?;
    card.configure_bus(timer).map_err(|_| "SD bus")?;
    let volume = SharedVolume::new(
        Fat32::mount(Partition::mbr(SdDevice::new(card, timer), 0).map_err(|_| "MBR")?)
            .map_err(|_| "FAT")?,
    );
    let mut music = SharedFile::open(&volume, "SATIE.WAV").map_err(|_| "WAV open")?;
    let wave = Wave::parse(&mut music).map_err(|_| "WAV header")?;
    if wave.sample_rate != 16000 {
        return Err("WAV rate");
    }
    let mut image = SharedFile::open(&volume, "HOME.JPG").map_err(|_| "JPEG open")?;
    let mut tx = music
        .with_timer(|t| unsafe { Tx::new(&[0], t) })
        .map_err(|_| "TX init")?;
    if music.with_timer(|t| tx.start(t)).is_err() {
        let _ = music.with_timer(|t| tx.stop(t));
        return Err("clock");
    }
    let mut codec = match music.with_timer(|t| Es8311::new(bus, t)) {
        Ok(c) => c,
        Err(_) => {
            let _ = music.with_timer(|t| tx.stop(t));
            return Err("codec");
        }
    };
    music.with_timer(|t| tx.stop(t)).map_err(|_| "TX stop")?;
    let tx = music
        .with_timer(|t| unsafe { Tx::new_stream(t) })
        .map_err(|_| "TX ring")?;
    let now = hal::time::cycle_count();
    let mut live = Live {
        tx,
        rx: None,
        pa: unsafe { hal::gpio::Output::<11>::new_low() },
        source: music,
        wave,
        // All arrays are initialized integer data in one exclusive 64B-aligned allocation.
        frames: unsafe { &mut *base.cast::<[u32; 512]>() },
        raw: unsafe { &mut *base.add(2048).cast::<[u8; 2048]>() },
        history: unsafe { core::slice::from_raw_parts_mut(base.add(4096).cast::<u32>(), HISTORY) },
        running: false,
        inject_repeat: repeat,
        repeat_at: None,
        produced: 0,
        index: 0,
        refills: 0,
        last_owned: [now; STREAM_BUFFERS],
        first_free: [0; STREAM_BUFFERS],
        lower: 0,
        upper: 0,
        min_owned: 4,
        underruns: 0,
        tx_errors: 0,
        last_poll: now,
        max_gap: 0,
        max_fill: 0,
        aligned: None,
        checked: 0,
        captured: 0,
        prefix: 0,
        mismatch: None,
        failed: None,
    };
    let mut frames = 0u32;
    let started = live.source.with_timer(|t| t.now_us());
    let cycles = hal::time::cycle_count();
    let result = (|| {
        for i in 0..STREAM_BUFFERS {
            if !live.fill(i) {
                return Err("prime");
            }
        }
        live.source
            .with_timer(|t| live.tx.start(t))
            .map_err(|_| "start")?;
        live.running = true;
        if capture_enabled {
            live.rx = Some(
                live.source
                    .with_timer(|t| unsafe {
                        Ring::start(base as usize + RX_OFFSET, base as usize + DESC_OFFSET, t)
                    })
                    .map_err(|_| "RX start")?,
            );
        }
        live.source
            .with_timer(|t| codec.mute(bus, t, false))
            .map_err(|_| "unmute")?;
        live.pa.set_high();
        let _ = writeln!(
            console,
            "WAV LAB START rate=16000 DAC=-21dB cache={:x?}",
            hal::memory::cache_snapshot()
        );
        loop {
            live.pump();
            if live.failed.is_some() {
                return Err("service");
            }
            let mut hash = 0x811c9dc5u32;
            let mut profile = jpeg::Profile::disabled().with_idct(Kind::Loeffler);
            let decoded = jpeg::decode_profiled(
                &mut image,
                1024,
                600,
                |x, y, pixels| {
                    fb.write_run(x, y, pixels);
                    for &pixel in pixels {
                        hash = (hash ^ u32::from(pixel)).wrapping_mul(0x01000193);
                    }
                    if y % 16 == 0 {
                        live.pump();
                    }
                },
                &mut profile,
            );
            decoded.map_err(|_| "JPEG decode")?;
            fb.finish_writes();
            frames += 1;
            if live.failed.is_some() {
                return Err("service");
            }
            if hash != 0x534b3012 || !fb.guards_ok() {
                return Err("JPEG hash/guard");
            }
            if frames.is_multiple_of(5) {
                let _ = writeln!(
                    console,
                    "WAV LAB frames={} produced={} verified={} owned_min={}",
                    frames, live.produced, live.checked, live.min_owned
                );
            }
            if stall {
                live.source.with_timer(|t| t.delay_us(200_000));
                live.pump();
                return Err("injected");
            }
            if (!capture_enabled && frames >= 8)
                || live.source.with_timer(|t| t.now_us()) - started >= 60_000_000
            {
                break;
            }
            if console.read_byte() == Some(27) {
                return Err("host stop");
            }
        }
        if capture_enabled && (live.checked < 100_000 || live.aligned.is_none()) {
            return Err("capture coverage");
        }
        Ok(())
    })();
    live.pa.set_low();
    let muted = live.source.with_timer(|t| codec.mute(bus, t, true));
    if let Some(rx) = live.rx.as_mut() {
        live.source
            .with_timer(|t| rx.stop(t))
            .map_err(|_| "RX stop")?;
    }
    let stopped = live.source.with_timer(|t| live.tx.stop(t));
    if stopped.is_err() && live.tx.status().link & 8 == 0 {
        panic!("TX not parked");
    }
    let elapsed = live.source.with_timer(|t| t.now_us()) - started;
    let cycle_span = hal::time::cycle_count() - cycles;
    let us = |ticks: u64| ticks * elapsed / cycle_span.max(1);
    let guards = [RX_OFFSET - 64, RX_OFFSET + RX_BYTES, TOTAL - 64]
        .into_iter()
        .all(|offset| {
            (0..64)
                .all(|i| unsafe { ((base as usize + offset + i + 0x4000_0000) as *const u8).read_volatile() } == 0xa5)
        });
    let _ = writeln!(console, "WAV LAB REPORT frames={} us={} refills={} owned_min={} latency_lower_us={} latency_upper_us={} fill_us={} gap_us={} underruns={} tx_errors={} captured={} verified={} prefix={} expected_next={:?} failure={:?} mismatch={:?} repeat_at={:?} guards={} tx={:?}",
        frames, elapsed, live.refills, live.min_owned, us(live.lower), us(live.upper), us(live.max_fill), us(live.max_gap), live.underruns, live.tx_errors, live.captured, live.checked, live.prefix, live.aligned, live.failed, live.mismatch, live.repeat_at, guards, live.tx.status());
    if !guards {
        return Err("RX guard");
    }
    if repeat
        && live.failed == Some("sample sequence")
        && live.tx_errors == 0
        && live.repeat_at.is_some()
        && live.repeat_at == live.mismatch.map(|m| m.0)
    {
        return Ok(());
    }
    if stall && live.underruns == 1 && live.failed == Some("tx") {
        return Ok(());
    }
    stopped.map_err(|_| "TX stop")?;
    muted.map_err(|_| "mute")?;
    result
}
