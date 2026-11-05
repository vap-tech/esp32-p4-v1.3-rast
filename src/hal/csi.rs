//! Polling CSI-2 / DW-GDMA bring-up for ESP32-P4 hw_ver1.
//! Standalone adaptation of ESP-IDF v6.1 CSI/DW-GDMA HAL (Apache-2.0).
use super::time::Timer;
use core::{arch::asm, marker::PhantomData};

const HOST: usize = 0x5009_f000;
const BRIDGE: usize = 0x5009_f800;
const DMA: usize = 0x5008_1000;
const ISP: usize = 0x500a_1000;
const CLOCK: usize = 0x500e_6000;
const US_MASK: u64 = (1 << 48) - 1;
const ERROR_REGS: [usize; 10] = [
    0xe0, 0xf0, 0x110, 0x280, 0x290, 0x2a0, 0x2b0, 0x2c0, 0x2d0, 0xc,
];

#[derive(Debug)]
pub struct Status {
    pub version: u32,
    pub isp: u32,
    pub phy: u32,
    pub stop: u32,
    pub errors: [u32; 10],
    pub bridge: u32,
    pub depth: u32,
    pub dma: u32,
    pub transferred: u32,
    pub destination: u32,
}

#[derive(Debug)]
pub enum Error {
    Timeout,
    Dma,
    InvalidBuffer,
    CacheTimeout,
    Receiver,
}

pub struct Csi {
    _exclusive: PhantomData<*mut ()>,
    buffer: usize,
    len: usize,
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

impl Csi {
    /// # Safety
    /// Own CSI host/bridge, ISP input, cache sync registers and DW-GDMA channel 0.
    /// Channel 1 can concurrently belong to DSI; neither driver resets DMA globally.
    /// LDO3 must already supply 2.5 V (the existing Display initializes it).
    /// ROM's PLL_F20M must be active. PSRAM mappings must stay stable when used.
    /// The ROM-provided L1 DCache for SRAM is synchronized explicitly.
    pub unsafe fn new(timer: &mut Timer) -> Self {
        unsafe {
            modify(CLOCK + 0x18, 0, (1 << 10) | (1 << 11) | (1 << 5));
            modify(CLOCK + 0x14, 0, 1 << 13);
            let reset = (1 << 27) | (1 << 28);
            modify(CLOCK + 0xc0, 0, reset);
            modify(CLOCK + 0xc0, reset, 0);
            modify(CLOCK + 0x3c, (3 << 2) | (1 << 4), 0);
            modify(CLOCK + 0x3c, 0, 1 << 4);
            // Rev1.x RAW bypass still needs the ISP input module configured.
            modify(CLOCK + 0xa8, 3 << 23, (1 << 23) | (1 << 25));
            modify(CLOCK + 0xac, 0xff, 2); // PLL240 / 3 = 80 MHz.
            modify(CLOCK + 0xc0, 0, 1 << 29);
            modify(CLOCK + 0xc0, 1 << 29, 0);
            write(ISP + 4, 1);
            write(ISP + 8, 1); // MIPI data enabled, ISP pipeline bypassed.
            write(ISP + 0x6c, 0);
            write(ISP + 0x70, 0x1fff_ffff);
            write(BRIDGE + 0x40, 3);
            write(HOST + 0x40, 0);
            write(HOST + 0x44, 0);
            write(HOST + 8, 0);
            write(HOST + 0x54, 1 << 16);
            write(HOST + 0x50, 1);
            write(HOST + 0x50, 0);
        }
        let mut this = Self {
            _exclusive: PhantomData,
            buffer: 0,
            len: 0,
        };
        // Nominal reference setting: 24-MHz sensor oscillator, ~400 Mbps/lane.
        this.phy_write(0x44, 0x25 << 1);
        unsafe {
            write(HOST + 4, 1); // Two lanes = N_LANES + 1.
            write(HOST + 0xc8, 1); // No extended VC.
            write(HOST + 0x300, 0); // No scrambling.
            write(HOST + 0x40, 1);
            write(HOST + 0x44, 1);
            write(HOST + 8, 1);
            write(BRIDGE, 1);
            write(BRIDGE + 4, 0);
            write(BRIDGE + 0x28, 0); // Poll raw status, no CPU interrupt.
            write(DMA + 0x10, 1); // DMAC enabled, global IRQ disabled.
        }
        timer.delay_us(1_000);
        this
    }

    fn phy_write(&mut self, reg: u8, value: u8) {
        unsafe {
            write(HOST + 0x50, 0);
            write(HOST + 0x54, (1 << 16) | u32::from(reg));
            write(HOST + 0x50, 2);
            write(HOST + 0x50, 0);
            write(HOST + 0x54, u32::from(value));
            write(HOST + 0x50, 2);
            write(HOST + 0x50, 0);
        }
    }

    /// Arm a single fixed-length RAW10 capture. Buffer must stay exclusively
    /// borrowed until `finish`, which stops channel 0 before returning on every path.
    ///
    /// # Safety
    /// `buffer` is 64-byte aligned, valid writable L2 SRAM for `len` bytes and
    /// remains exclusively owned until finish or stop; no CPU reads/writes while
    /// DMA is active. Own all corresponding cache lines. Sensor emits RAW10 at
    /// the requested dimensions and has completed its startup warmup.
    /// Do not arm another transfer before finishing/stopping the previous one.
    pub unsafe fn arm(
        &mut self,
        buffer: *mut u8,
        len: usize,
        width: u16,
        height: u16,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        let start = buffer as usize;
        if !(16..=4095).contains(&width)
            || !width.is_multiple_of(16)
            || height == 0
            || height > 4095
            || start & 63 != 0
            || len == 0
            || len & 127 != 0
            || start < 0x4ff0_0000
            || start.checked_add(len).is_none_or(|end| end > 0x4ff2_8000)
            || len != usize::from(width) * usize::from(height) * 10 / 8
        {
            return Err(Error::InvalidBuffer);
        }
        self.buffer = start;
        self.len = len;
        self.cache_sync(8, timer)?; // L1 writeback + invalidate before DMA.
        unsafe {
            write(BRIDGE + 4, 0);
            // Flush old frame fragments and ISP input state after sensor warmup.
            let reset = (1 << 28) | (1 << 29);
            modify(CLOCK + 0xc0, 0, reset);
            modify(CLOCK + 0xc0, reset, 0);
            write(BRIDGE, 1);
            write(BRIDGE + 0x40, 3);
            write(ISP + 4, 1);
            write(ISP + 8, 1);
            write(ISP + 0x6c, 0);
            let _ = ERROR_REGS.map(|off| read(HOST + off));
            write(BRIDGE + 0x0c, 960);
            write(BRIDGE + 0x10, 0x2b2b); // RAW10 packets only.
            write(BRIDGE + 0x14, (u32::from(width) << 12) | u32::from(height));
            write(BRIDGE + 0x18, 0);
            write(
                ISP + 0x10,
                ((u32::from(width) * 10 / 32 - 1) << 12) | (u32::from(height) - 1),
            );
            write(ISP + 0x70, 0x1fff_ffff);
            write(BRIDGE + 8, 16); // Sixteen 64-bit words per DMA request.
            write(BRIDGE + 0x2c, 1);
            write(BRIDGE + 0x30, (len / 128 - 1) as u32);
            write(BRIDGE + 0x20, 0x3f);
            write(DMA + 0x100, 0x5010_4000); // CSI bridge FIFO.
            write(DMA + 0x104, 0);
            write(DMA + 0x108, start as u32);
            write(DMA + 0x10c, 0);
            write(DMA + 0x110, (len / 8 - 1) as u32);
            // M0 peripheral -> M1 memory; fixed source, incrementing destination,
            // 64-bit items, 16-item bursts. DMA controls length (bounded writes).
            write(
                DMA + 0x118,
                (1 << 30) | (1 << 2) | (1 << 4) | (3 << 8) | (3 << 11) | (3 << 14) | (3 << 18),
            );
            write(DMA + 0x11c, (1 << 6) | (15 << 7) | (1 << 15) | (15 << 16));
            write(DMA + 0x120, 0); // Single contiguous block.
            write(
                DMA + 0x124,
                2 | (1 << 7) | (1 << 17) | (4 << 23) | (4 << 27),
            );
            write(DMA + 0x180, u32::MAX);
            write(DMA + 0x190, 0);
            write(DMA + 0x198, u32::MAX);
            asm!("fence rw, rw", options(nostack));
            write(DMA + 0x18, 0x101);
            write(BRIDGE + 4, 1);
        }
        Ok(())
    }

    /// Direct RAW10 -> ISP RGB565 -> one PSRAM framebuffer. Both DMA endpoints
    /// reload at each frame; DSI channel 1 continues to read that same buffer.
    /// # Safety
    /// `address` is a valid 1024x600 RGB565 PSRAM DMA mapping, kept alive until
    /// stop(). CPU must use the uncached alias; single buffer allows tearing.
    pub unsafe fn start_rgb565(&mut self, address: usize) -> Result<(), Error> {
        if address != 0x4800_0080 {
            return Err(Error::InvalidBuffer);
        }
        self.stop();
        unsafe {
            modify(CLOCK + 0xac, 0xff, 1); // ISP PLL240 / 2 = 120 MHz.
            let reset = (1 << 28) | (1 << 29);
            modify(CLOCK + 0xc0, 0, reset);
            modify(CLOCK + 0xc0, reset, 0);
            write(BRIDGE, 1);
            write(BRIDGE + 0x40, 3);
            write(ISP + 4, 1);
            write(ISP + 0x6c, 0);
            // MIPI, RAW10, RGB565 output; demosaic and RGB/YUV roundtrip.
            write(
                ISP + 8,
                1 | (1 << 25) | (4 << 29) | (1 << 6) | (1 << 10) | (1 << 13),
            );
            write(ISP + 0x10, (1 << 27) | (1023 << 12) | 599); // GBRG, no LS/LE.
            write(ISP + 0x234, 0); // BT601 full range.
            write(ISP + 0x70, 0x1fff_ffff);
            let _ = ERROR_REGS.map(|off| read(HOST + off));
            write(BRIDGE + 0x0c, 960);
            write(BRIDGE + 0x10, 0x2f12);
            write(BRIDGE + 0x14, (1024 << 12) | 600);
            write(BRIDGE + 0x18, 0);
            write(BRIDGE + 8, 128);
            write(BRIDGE + 0x2c, 1);
            write(BRIDGE + 0x30, 1199);
            write(BRIDGE + 0x20, 0x3f);
            write(DMA + 0x100, 0x5010_4000);
            write(DMA + 0x104, 0);
            write(DMA + 0x108, address as u32);
            write(DMA + 0x10c, 0);
            write(DMA + 0x110, 153599);
            write(
                DMA + 0x118,
                (1 << 30) | (1 << 2) | (1 << 4) | (3 << 8) | (3 << 11) | (6 << 14) | (6 << 18),
            );
            write(DMA + 0x11c, (1 << 6) | (15 << 7) | (1 << 15) | (15 << 16));
            write(DMA + 0x120, 5); // Source/destination auto reload.
            write(
                DMA + 0x124,
                2 | (1 << 7) | (1 << 17) | (4 << 23) | (4 << 27),
            );
            write(DMA + 0x180, u32::MAX);
            write(DMA + 0x190, 0);
            write(DMA + 0x198, u32::MAX);
            asm!("fence rw, rw", options(nostack));
            write(DMA + 0x18, 0x101);
            write(BRIDGE + 4, 1);
            modify(ISP + 8, 0, 1 << 1); // Enable ISP after DMA is ready.
        }
        Ok(())
    }

    /// Read and clear per-window live stream diagnostics.
    pub fn live_status(&mut self) -> Status {
        let status = self.status();
        unsafe {
            write(BRIDGE + 0x20, 0x3f);
            write(ISP + 0x70, 0x1fff_ffff);
        }
        status
    }

    /// Diagnostic snapshot; reading host error registers clears their flags.
    pub fn status(&mut self) -> Status {
        unsafe {
            Status {
                version: read(HOST),
                isp: read(ISP + 0x64),
                phy: read(HOST + 0x48),
                stop: read(HOST + 0x4c),
                errors: ERROR_REGS.map(|off| read(HOST + off)),
                bridge: read(BRIDGE + 0x1c),
                depth: read(BRIDGE + 0x0c) >> 16,
                dma: read(DMA + 0x188),
                transferred: read(DMA + 0x130),
                destination: read(DMA + 0x108),
            }
        }
    }

    /// Wait at most 500 ms. No USB writes while hardware owns the buffer.
    /// Always stop bridge and DMA channel 0, including on timeout.
    pub fn finish(&mut self, timer: &mut Timer) -> (Result<(), Error>, Status) {
        let start = timer.now_us();
        let result = loop {
            let flags = unsafe { read(DMA + 0x188) };
            if flags & 0x3fe0 != 0 {
                break Err(Error::Dma);
            }
            if flags & 2 != 0 {
                break Ok(());
            }
            if timer.now_us().wrapping_sub(start) & US_MASK >= 500_000 {
                break Err(Error::Timeout);
            }
        };
        let status = self.status();
        let result = result.and_then(|()| {
            if status.errors.iter().any(|&e| e != 0)
                || status.bridge & 0x1f != 0
                || status.isp & 0x3f != 0
            {
                Err(Error::Receiver)
            } else {
                Ok(())
            }
        });
        self.stop();
        let sync = self.cache_sync(1, timer); // Invalidate DMA destination.
        (result.and(sync), status)
    }

    fn cache_sync(&mut self, operation: u32, timer: &mut Timer) -> Result<(), Error> {
        const CACHE: usize = 0x3ff1_0000;
        unsafe {
            asm!("fence rw, rw", options(nostack));
            write(CACHE + 0x9c, 1 << 4); // L1 DCache, internal SRAM.
            write(CACHE + 0xa0, self.buffer as u32);
            write(CACHE + 0xa4, self.len as u32);
            write(CACHE + 0x98, operation);
        }
        let start = timer.now_us();
        while unsafe { read(CACHE + 0x98) } & operation != 0 {
            if timer.now_us().wrapping_sub(start) & US_MASK >= 50_000 {
                return Err(Error::CacheTimeout);
            }
        }
        unsafe {
            asm!("fence rw, rw", options(nostack));
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        unsafe {
            write(BRIDGE + 4, 0);
            // Keep DSI channel 1 running; stop/abort only CSI channel 0.
            if read(DMA + 0x18) & 1 != 0 {
                write(DMA + 0x1c, 0x101);
                let mut stopped = false;
                for _ in 0..20000 {
                    if read(DMA + 0x18) & 1 == 0 {
                        stopped = true;
                        break;
                    }
                }
                if !stopped {
                    panic!("CSI DMA abort timeout");
                }
            }
            write(DMA + 0x198, u32::MAX);
            asm!("fence rw, rw", options(nostack));
        }
    }
}
