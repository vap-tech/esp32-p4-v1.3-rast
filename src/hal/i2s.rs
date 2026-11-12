//! I2S1 TX on P4 rev1.x: XTAL40, 16 kHz, stereo PCM16 in 16-bit slots.
//! Dedicated AHB DMA TX channel 0, static cyclic buffer, polling, no interrupts.
use super::time::Timer;
use core::marker::PhantomData;
const I2S: usize = 0x500c_7000;
const DMA: usize = 0x5008_5000;
const CLOCK: usize = 0x500e_6000;
const GPIO: usize = 0x500e_0000;
const MCLK_N: u32 = 9;
const MCLK_A: u32 = 64;
const MCLK_B: u32 = 49;
const FRAC_YN1: u32 = (MCLK_B * 2 > MCLK_A) as u32;
const FRAC_Z: u32 = if FRAC_YN1 != 0 {
    MCLK_A - MCLK_B
} else {
    MCLK_B
};
const FRAC_X: u32 = MCLK_A / FRAC_Z - 1;
const FRAC_Y: u32 = MCLK_A % FRAC_Z;
const BCLK_DIV: u32 = 8;
const SLOT_BITS: u32 = 16;
const DATA_BITS: u32 = 16;
const TX_CONF: u32 =
    (1 << 12) | (1 << 13) | (1 << 14) | (1 << 15) | (1 << 19) | (1 << 30) | ((BCLK_DIV - 1) << 21);
const MASK: u64 = (1 << 48) - 1;
pub const STREAM_BUFFERS: usize = 4;
#[repr(C, align(64))]
struct Buffer([u32; 512 * STREAM_BUFFERS]);
#[repr(C, align(64))]
struct Descriptor([u32; 16]);
static mut BUFFER: Buffer = Buffer([0; 512 * STREAM_BUFFERS]);
static mut STREAM_DESC: [Descriptor; STREAM_BUFFERS] =
    [const { Descriptor([0; 16]) }; STREAM_BUFFERS];
static mut DESC: Descriptor = Descriptor([0; 16]);
#[repr(C, align(64))]
struct CaptureBuffer([u32; 64]);
static mut RX_BUFFER: CaptureBuffer = CaptureBuffer([0; 64]);
static mut RX_DESC: Descriptor = Descriptor([0; 16]);
#[derive(Debug)]
pub struct Capture {
    pub words: [u32; 64],
    pub dma: u32,
    pub descriptor: u32,
}

#[derive(Debug)]
pub enum Error {
    Timeout(&'static str),
    Dma(u32),
    InvalidBuffer,
}
#[derive(Debug)]
pub struct Status {
    pub fifo: u32,
    pub bck: u32,
    pub dma: u32,
    pub i2s: u32,
    pub link: u32,
    pub descriptor: u32,
    pub config: u32,
    pub owners: [[u32; 3]; STREAM_BUFFERS],
}
pub struct Tx {
    _exclusive: PhantomData<*mut ()>,
}
unsafe fn read(addr: usize) -> u32 {
    unsafe { (addr as *const u32).read_volatile() }
}
unsafe fn write(addr: usize, value: u32) {
    unsafe { (addr as *mut u32).write_volatile(value) }
}
unsafe fn modify(addr: usize, clear: u32, set: u32) {
    unsafe { write(addr, (read(addr) & !clear) | set) }
}
fn wait(
    addr: usize,
    mask: u32,
    value: u32,
    timer: &mut Timer,
    what: &'static str,
) -> Result<(), Error> {
    let start = timer.now_us();
    while unsafe { read(addr) } & mask != value {
        if timer.now_us().wrapping_sub(start) & MASK > 100_000 {
            return Err(Error::Timeout(what));
        }
    }
    Ok(())
}
fn sync(addr: usize, len: u32, _timer: &mut Timer) -> Result<(), Error> {
    sync_now(addr, len)
}
// Polled DMA service does not borrow the timer held by an SD file. Bound cache
// maintenance by 16M CPU cycles (100ms at the measured 160MHz), not an IRQ/runtime.
fn sync_now(addr: usize, len: u32) -> Result<(), Error> {
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
        write(0x3ff1_009c, 1 << 4);
        write(0x3ff1_00a0, addr as u32);
        write(0x3ff1_00a4, len);
    }
    for _ in 0..2 {
        unsafe {
            write(0x3ff1_0098, 8);
        }
        let start = super::time::cycle_count();
        while unsafe { read(0x3ff1_0098) } & 16 == 0 {
            if super::time::cycle_count().wrapping_sub(start) > 16_000_000 {
                return Err(Error::Timeout("cache"));
            }
        }
    }
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
    }
    Ok(())
}

impl Tx {
    /// # Safety
    /// Exclusive I2S1, GPIO9/10/12/13/48, AHB DMA pair 0 (TX/RX) and static audio DMA storage.
    /// No other core/ISR may access those resources or cache-sync registers.
    /// The external PA must remain disabled while initializing/stopping.
    /// Frames are copied to owned SRAM: each word packs left/right signed PCM16
    /// in little-endian order. 1..=512 frames repeat until stop().
    pub unsafe fn new(frames: &[u32], timer: &mut Timer) -> Result<Self, Error> {
        if frames.is_empty() || frames.len() > 512 {
            return Err(Error::InvalidBuffer);
        }
        let bytes = (frames.len() * 4) as u32;
        unsafe {
            modify(CLOCK + 0x1c, 0, 1 << 15);
            modify(0x5011_1040, 0, 1 << 11);
            modify(CLOCK + 0xc8, 0, 1 << 5);
            modify(CLOCK + 0xc8, 1 << 5, 0);
            modify(CLOCK + 0x54, 3 << 29, 1 << 28); // TX clock source XTAL40.
                                                    // SDK workaround: switch to a small divider, then set fractional
                                                    // fields in order. 40MHz / (9 + 49/64) = 4.096MHz MCLK.
            modify(CLOCK + 0x58, 255, 2);
            modify(CLOCK + 0x5c, 1 << 9, 0);
            modify(CLOCK + 0x58, 511 << 17, 1 << 17);
            modify(CLOCK + 0x5c, 511, 0);
            modify(CLOCK + 0x58, 511 << 8, 0);
            modify(CLOCK + 0x5c, 1 << 9, FRAC_YN1 << 9);
            modify(CLOCK + 0x5c, 511, FRAC_Z);
            modify(CLOCK + 0x58, 511 << 17, FRAC_Y << 17);
            modify(CLOCK + 0x58, 511 << 8, FRAC_X << 8);
            modify(CLOCK + 0x58, 255, MCLK_N);
            modify(CLOCK + 0x5c, 0, 1 << 10); // MCLK bound to TX.
            write(I2S + 0x24, TX_CONF | 3);
            write(I2S + 0x24, TX_CONF);
            write(
                I2S + 0x2c,
                (SLOT_BITS - 1)
                    | ((DATA_BITS - 1) << 14)
                    | ((SLOT_BITS - 1) << 19)
                    | ((SLOT_BITS - 1) << 27),
            );
            write(I2S + 0x54, 3 | (1 << 16)); // Both slots, 2 channels.
            write(I2S + 0x40, 0);
            write(I2S + 0x5c, 0);
            write(I2S + 0x68, 0);
            write(I2S + 0x14, 0);
            write(I2S + 0x18, u32::MAX);
            for (pin, signal) in [(9usize, 34), (10, 33), (12, 31), (13, 32)] {
                write(GPIO + 0x28, 1 << pin);
                modify(GPIO + 0x74 + pin * 4, 1 << 2, 0);
                write(GPIO + 0x558 + pin * 4, signal | (1 << 10));
                modify(
                    0x500e_1004 + pin * 4,
                    (7 << 12) | (1 << 7) | (1 << 8),
                    (1 << 12) | (1 << 9),
                );
                write(GPIO + 0x24, 1 << pin);
            }
            // This is AHB GDMA, independent of DW-GDMA used by DSI/CSI.
            modify(CLOCK + 0x18, 0, 1 << 3);
            modify(DMA + 0x64, 0, 1 << 3);
            write(DMA + 0xe0, 1); // Stop this channel, never reset the whole DMA unit.
            write(DMA + 0xd0, 1);
            write(DMA + 0xd0, (1 << 3) | (1 << 4) | (1 << 5));
            write(DMA + 0xd4, 1 << 12); // Owner check; auto writeback off for cyclic ROM-like data.
            write(DMA + 0x38, 0);
            write(DMA + 0x3c, u32::MAX);
            write(DMA + 0xfc, 1);
            write(DMA + 0x100, 4); // I2S1 trigger.
        }
        wait(DMA + 0xe0, 8, 8, timer, "DMA reset park")?;
        let data = core::ptr::addr_of_mut!(BUFFER);
        let desc = core::ptr::addr_of_mut!(DESC);
        unsafe {
            (&mut (*data).0)[..frames.len()].copy_from_slice(frames);
            (*desc).0.fill(0);
            (*desc).0[0] = bytes | (bytes << 12) | (1 << 30) | (1 << 31);
            (*desc).0[1] = data as u32;
            (*desc).0[2] = desc as u32;
        }
        sync(data as usize, bytes.next_multiple_of(64), timer)?;
        sync(desc as usize, 64, timer)?;
        unsafe {
            write(DMA + 0x3b8, desc as u32);
        }
        Ok(Self {
            _exclusive: PhantomData,
        })
    }
    /// Four 512-frame buffers (128 ms at 16 kHz). Same exclusive resources as new().
    /// Starts with silence; prime each buffer with stream_refill before start().
    /// # Safety
    /// See new(). No cyclic tone or other TX owner may coexist.
    pub unsafe fn new_stream(timer: &mut Timer) -> Result<Self, Error> {
        let this = unsafe { Self::new(&[0; 1], timer) }?;
        let data = core::ptr::addr_of_mut!(BUFFER).cast::<u32>();
        let descs = core::ptr::addr_of_mut!(STREAM_DESC).cast::<Descriptor>();
        unsafe {
            for i in 0..512 * STREAM_BUFFERS {
                data.add(i).write(0);
            }
            for i in 0..STREAM_BUFFERS {
                (*descs.add(i)).0.fill(0);
                (*descs.add(i)).0[0] = 2048 | (2048 << 12) | (1 << 30); // CPU owns until primed.
                (*descs.add(i)).0[1] = data.add(i * 512) as u32;
                (*descs.add(i)).0[2] = descs.add((i + 1) % STREAM_BUFFERS) as u32;
            }
        }
        sync(data as usize, (2048 * STREAM_BUFFERS) as u32, timer)?;
        sync(descs as usize, (STREAM_BUFFERS * 64) as u32, timer)?;
        unsafe {
            modify(DMA + 0xd0, 0, 1 << 2); // Auto writeback clears OWN after each buffer.
            write(DMA + 0x3b8, descs as u32);
        }
        Ok(this)
    }
    /// True after DMA releases this buffer. Descriptor lines never share CPU data.
    pub fn stream_ready(&mut self, index: usize, _timer: &mut Timer) -> Result<bool, Error> {
        self.stream_released(index)
    }
    /// Poll buffer ownership without borrowing SYSTIMER from a file transport.
    pub fn stream_released(&mut self, index: usize) -> Result<bool, Error> {
        if index >= STREAM_BUFFERS {
            return Err(Error::InvalidBuffer);
        }
        let ints = unsafe { read(DMA + 0x30) };
        if ints & ((1 << 2) | (1 << 4) | (1 << 5)) != 0 {
            return Err(Error::Dma(ints));
        }
        // DMA writes OWN asynchronously. Use the uncached SRAM alias after the
        // initial C2M flush; repeated cache snapshots can hide a live writeback.
        let desc = (core::ptr::addr_of_mut!(STREAM_DESC) as usize + 0x4000_0000) as *mut Descriptor;
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
        Ok(unsafe { (*desc.add(index)).0.as_ptr().read_volatile() } & (1 << 31) == 0)
    }

    /// Replace a released buffer, then transfer ownership back to DMA.
    pub fn stream_refill(
        &mut self,
        index: usize,
        frames: &[u32; 512],
        timer: &mut Timer,
    ) -> Result<(), Error> {
        let _ = timer;
        self.stream_refill_now(index, frames)
    }
    /// Refill a released block from a cooperative event loop or renderer callback.
    pub fn stream_refill_now(&mut self, index: usize, frames: &[u32; 512]) -> Result<(), Error> {
        if !self.stream_released(index)? {
            return Err(Error::InvalidBuffer);
        }
        let data = unsafe {
            core::ptr::addr_of_mut!(BUFFER)
                .cast::<u32>()
                .add(index * 512)
        };
        let desc = unsafe {
            ((core::ptr::addr_of_mut!(STREAM_DESC) as usize + 0x4000_0000) as *mut Descriptor)
                .add(index)
        };
        unsafe {
            core::ptr::copy_nonoverlapping(frames.as_ptr(), data, 512);
        }
        sync_now(data as usize, 2048)?;
        unsafe {
            (*desc)
                .0
                .as_mut_ptr()
                .write_volatile(2048 | (2048 << 12) | (1 << 30) | (1 << 31));
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
        Ok(())
    }
    /// Nonblocking stop request. Keep resources alive until stop() confirms park.
    pub fn request_stop(&mut self) {
        unsafe {
            write(I2S + 0x24, TX_CONF);
            write(DMA + 0xe0, 1);
        }
    }
    /// Non-atomic physical snapshot; includes in-flight/prefetched descriptors.
    pub fn stream_owned_mask(&self) -> u8 {
        let base = (core::ptr::addr_of!(STREAM_DESC) as usize + 0x4000_0000) as *const Descriptor;
        let mut mask = 0;
        for i in 0..STREAM_BUFFERS {
            mask |= ((unsafe { (*base.add(i)).0.as_ptr().read_volatile() } >> 31) as u8) << i;
        }
        mask
    }
    pub fn start(&mut self, timer: &mut Timer) -> Result<(), Error> {
        unsafe {
            write(I2S + 0x74, 1 << 31);
            write(I2S + 0x78, 1 << 31);
            write(DMA + 0xe0, 2);
            write(I2S + 0x24, TX_CONF | (1 << 8));
        }
        wait(I2S + 0x24, 1 << 8, 0, timer, "I2S update")?;
        unsafe {
            write(I2S + 0x24, TX_CONF | 4);
        }
        Ok(())
    }
    /// Short RX diagnostic using TX's shared BCLK/WS: sample GPIO9 (our own
    /// DOUT) or GPIO48 (codec SDOUT). Requires exclusive RX0 and GPIO48 ownership.
    pub fn capture(&mut self, pin: u8, timer: &mut Timer) -> Result<Capture, Error> {
        if pin != 9 && pin != 48 {
            return Err(Error::InvalidBuffer);
        }
        const RX_CONF: u32 =
            (1 << 3) | (1 << 12) | (1 << 13) | (1 << 15) | (1 << 19) | ((BCLK_DIV - 1) << 21);
        unsafe {
            modify(
                CLOCK + 0x50,
                (3 << 21) | (255 << 23),
                (1 << 20) | (MCLK_N << 23),
            );
            modify(
                CLOCK + 0x54,
                0x0fff_ffff,
                FRAC_X | (FRAC_Y << 9) | (FRAC_Z << 18) | (FRAC_YN1 << 27),
            );
            modify(I2S + 0x24, 0, 1 << 30); // Internal shared TX BCLK/WS to RX.
            if pin == 48 {
                write(GPIO + 0x34, 1 << 16);
            }
            modify(
                0x500e_1004 + usize::from(pin) * 4,
                7 << 12,
                (1 << 12) | (1 << 9),
            );
            write(GPIO + 0x158 + 34 * 4, u32::from(pin) | (1 << 7));
            write(I2S + 0x20, RX_CONF | 3);
            write(I2S + 0x20, RX_CONF);
            write(
                I2S + 0x28,
                (SLOT_BITS - 1)
                    | ((DATA_BITS - 1) << 14)
                    | ((SLOT_BITS - 1) << 19)
                    | ((SLOT_BITS - 1) << 27),
            );
            write(I2S + 0x50, 3 | (1 << 16));
            write(I2S + 0x48, 0);
            write(I2S + 0x58, 0);
            write(I2S + 0x64, 256);
            write(DMA + 0x80, 2);
            write(DMA + 0x70, 1);
            write(DMA + 0x70, 0);
            write(DMA + 0x74, 1 << 12);
            write(DMA + 0x8, 0);
            write(DMA + 0xc, u32::MAX);
            write(DMA + 0xa0, 4);
            write(DMA + 0x9c, 1);
        }
        wait(DMA + 0x80, 16, 16, timer, "RX reset park")?;
        let data = core::ptr::addr_of_mut!(RX_BUFFER);
        let desc = core::ptr::addr_of_mut!(RX_DESC);
        unsafe {
            (*data).0.fill(0xa5a5a5a5);
            (*desc).0.fill(0);
            (*desc).0[0] = 256 | (1 << 31);
            (*desc).0[1] = data as u32;
        }
        sync(data as usize, 256, timer)?;
        sync(desc as usize, 64, timer)?;
        let result = (|| {
            unsafe {
                write(DMA + 0x3ac, desc as u32);
                write(DMA + 0x80, 4);
                write(I2S + 0x20, RX_CONF | (1 << 8));
            }
            wait(I2S + 0x20, 1 << 8, 0, timer, "RX update")?;
            unsafe {
                write(I2S + 0x20, RX_CONF | 4);
            }
            let started = timer.now_us();
            loop {
                let ints = unsafe { read(DMA) };
                if ints & ((1 << 2) | (1 << 3)) != 0 {
                    return Err(Error::Dma(ints));
                }
                if ints & 2 != 0 {
                    return Ok(ints);
                }
                if timer.now_us().wrapping_sub(started) & MASK > 100_000 {
                    return Err(Error::Timeout("RX EOF"));
                }
            }
        })();
        unsafe {
            write(I2S + 0x20, RX_CONF);
            write(DMA + 0x80, 2);
        }
        if wait(DMA + 0x80, 16, 16, timer, "RX stop").is_err() {
            unsafe {
                write(DMA + 0x70, 1);
                write(DMA + 0x70, 0);
            }
            wait(DMA + 0x80, 16, 16, timer, "RX reset after stop")?;
        }
        let ints = result?;
        unsafe {
            write(0x3ff1_009c, 1 << 4);
            write(0x3ff1_00a0, data as u32);
            write(0x3ff1_00a4, 256);
            write(0x3ff1_0098, 1);
        }
        wait(0x3ff1_0098, 1, 0, timer, "RX cache")?;
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
        let mut words = [0; 64];
        for (i, word) in words.iter_mut().enumerate() {
            *word = unsafe { (*data).0.as_ptr().add(i).read_volatile() };
        }
        // Descriptor is invalidated independently of the data.
        unsafe {
            write(0x3ff1_00a0, desc as u32);
            write(0x3ff1_00a4, 64);
            write(0x3ff1_0098, 1);
        }
        wait(0x3ff1_0098, 1, 0, timer, "RX descriptor cache")?;
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
        let descriptor = unsafe { (*desc).0.as_ptr().read_volatile() };
        if descriptor & (1 << 31) != 0 || (descriptor >> 12) & 0xfff != 256 {
            return Err(Error::Dma(ints));
        }
        Ok(Capture {
            words,
            dma: ints,
            descriptor,
        })
    }
    /// GPIO pad readback: high samples and transitions for DOUT/WS/BCLK/MCLK.
    /// A digital activity check, not a frequency measurement.
    pub fn pin_activity(&self) -> ([u32; 4], [u32; 4]) {
        let mut high = [0; 4];
        let mut edges = [0; 4];
        let mut previous = unsafe { read(GPIO + 0x3c) };
        for _ in 0..4096 {
            let sample = unsafe { read(GPIO + 0x3c) };
            for (i, pin) in [9, 10, 12, 13].iter().enumerate() {
                high[i] += (sample >> pin) & 1;
                edges[i] += ((sample ^ previous) >> pin) & 1;
            }
            previous = sample;
        }
        (high, edges)
    }
    pub fn status(&self) -> Status {
        unsafe {
            Status {
                fifo: read(I2S + 0x74),
                bck: read(I2S + 0x78),
                dma: read(DMA + 0x30),
                i2s: read(I2S + 0x0c),
                link: read(DMA + 0xe0),
                descriptor: read(DMA + 0xf0),
                config: read(DMA + 0xd0),
                owners: core::array::from_fn(|i| {
                    core::array::from_fn(|word| {
                        ((core::ptr::addr_of!(STREAM_DESC) as usize + 0x4000_0000)
                            as *const Descriptor)
                            .add(i)
                            .cast::<u32>()
                            .add(word)
                            .read_volatile()
                    })
                }),
            }
        }
    }
    pub fn stop(&mut self, timer: &mut Timer) -> Result<(), Error> {
        unsafe {
            write(I2S + 0x24, TX_CONF);
            write(DMA + 0xe0, 1);
        }
        let result = wait(DMA + 0xe0, 8, 8, timer, "DMA stop");
        if result.is_err() {
            unsafe {
                write(DMA + 0xd0, 1);
                write(DMA + 0xd0, (1 << 3) | (1 << 4) | (1 << 5));
            }
            wait(DMA + 0xe0, 8, 8, timer, "DMA reset after stop")?;
        }
        unsafe {
            modify(CLOCK + 0x54, 1 << 28, 0);
            modify(CLOCK + 0x50, 1 << 20, 0);
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xtal_fraction_and_frame_dividers_produce_16khz() {
        // Recover a/b from the actual hardware fields used above.
        let a = (FRAC_X + 1) * FRAC_Z + FRAC_Y;
        let b = if FRAC_YN1 != 0 { a - FRAC_Z } else { FRAC_Z };
        assert_eq!((a, b), (MCLK_A, MCLK_B));
        let mclk = 40_000_000u64 * u64::from(a) / u64::from(MCLK_N * a + b);
        assert_eq!(mclk, 4_096_000);
        assert_eq!(
            mclk / u64::from(BCLK_DIV) / u64::from(2 * SLOT_BITS),
            16_000
        );
        assert_eq!(DATA_BITS, 16);
    }
}

pub mod capture;
