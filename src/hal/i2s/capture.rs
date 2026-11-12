//! Continuous GPIO9/GPIO48 capture on AHB RX0. Caller retains the arena allocations
//! until stop() confirms park. CPU touches these buffers only through uncached aliases.
use super::*;
pub const BLOCKS: usize = 8;
pub const WORDS: usize = 512;
const RX_CONF: u32 =
    (1 << 3) | (1 << 12) | (1 << 13) | (1 << 15) | (1 << 19) | ((BCLK_DIV - 1) << 21);
pub struct Ring {
    data: usize,
    desc: usize,
    index: usize,
    active: bool,
}
impl Ring {
    /// # Safety
    /// Own RX0/GPIO9 input and the two disjoint, 64B-aligned SRAM allocations
    /// (BLOCKS*2048 data bytes and BLOCKS*64 descriptor bytes). TX owns the
    /// shared I2S clocks. No cached alias may be used until stop confirms park.
    pub unsafe fn start(data: usize, desc: usize, timer: &mut Timer) -> Result<Self, Error> {
        unsafe { Self::start_pin(data, desc, 9, timer) }
    }
    /// Start RX from loopback GPIO9 or codec SDOUT GPIO48.
    /// # Safety
    /// Same ownership/alignment contract as start(), plus exclusive selected GPIO input.
    pub unsafe fn start_pin(
        data: usize,
        desc: usize,
        pin: u8,
        timer: &mut Timer,
    ) -> Result<Self, Error> {
        if pin != 9 && pin != 48 {
            return Err(Error::InvalidBuffer);
        }

        if !super::super::memory::valid_ahb_range(data, BLOCKS * WORDS * 4)
            || !super::super::memory::valid_ahb_range(desc, BLOCKS * 64)
        {
            return Err(Error::InvalidBuffer);
        }
        sync(data, (BLOCKS * WORDS * 4) as u32, timer)?;
        sync(desc, (BLOCKS * 64) as u32, timer)?;
        let mut ring = Self {
            data,
            desc,
            index: 0,
            active: false,
        };
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
            modify(I2S + 0x24, 0, 1 << 30);
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
            write(I2S + 0x64, (WORDS * 4) as u32);
            write(DMA + 0x80, 2);
            write(DMA + 0x70, 1);
            write(DMA + 0x70, 0);
            write(DMA + 0x74, 1 << 12);
            write(DMA + 8, 0);
            write(DMA + 0xc, u32::MAX);
            write(DMA + 0xa0, 4);
            write(DMA + 0x9c, 1);
        }
        wait(DMA + 0x80, 16, 16, timer, "RX park").expect("RX DMA not parked");
        for i in 0..BLOCKS {
            let d = (desc + 0x4000_0000 + i * 64) as *mut u32;
            unsafe {
                for word in 0..16 {
                    d.add(word).write_volatile(0);
                }
                d.add(1).write_volatile((data + i * WORDS * 4) as u32);
                d.add(2)
                    .write_volatile((desc + ((i + 1) % BLOCKS) * 64) as u32);
                d.write_volatile((WORDS * 4) as u32 | (1 << 31));
            }
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
            write(DMA + 0x3ac, desc as u32);
            write(DMA + 0x80, 4);
            write(I2S + 0x20, RX_CONF | (1 << 8));
        }
        ring.active = true;
        if let Err(error) = wait(I2S + 0x20, 1 << 8, 0, timer, "RX update") {
            ring.stop(timer)?;
            return Err(error);
        }
        unsafe {
            write(I2S + 0x20, RX_CONF | 4);
        }
        Ok(ring)
    }
    pub fn ready(&self) -> Result<bool, Error> {
        let ints = unsafe { read(DMA) };
        if ints & 0x7c != 0 {
            return Err(Error::Dma(ints));
        }
        let header =
            unsafe { ((self.desc + 0x4000_0000 + self.index * 64) as *const u32).read_volatile() };
        if header >> 31 != 0 {
            return Ok(false);
        }
        if (header >> 12) & 0xfff != (WORDS * 4) as u32 {
            return Err(Error::Dma(header));
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
        Ok(true)
    }
    /// Read one released sample.
    /// # Safety
    /// Caller must have observed ready(); OWN remains CPU throughout inspection.
    pub unsafe fn word(&self, index: usize) -> u32 {
        assert!(index < WORDS);
        unsafe {
            ((self.data + 0x4000_0000 + self.index * WORDS * 4) as *const u32)
                .add(index)
                .read_volatile()
        }
    }
    /// # Safety
    /// The current block was observed ready and all CPU inspection is finished.
    pub unsafe fn release(&mut self) {
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
            ((self.desc + 0x4000_0000 + self.index * 64) as *mut u32)
                .write_volatile((WORDS * 4) as u32 | (1 << 31));
        }
        self.index = (self.index + 1) % BLOCKS;
    }
    pub fn stop(&mut self, timer: &mut Timer) -> Result<(), Error> {
        if !self.active {
            return Ok(());
        }
        unsafe {
            write(I2S + 0x20, RX_CONF);
            write(DMA + 0x80, 2);
        }
        if wait(DMA + 0x80, 16, 16, timer, "RX stop").is_err() {
            unsafe {
                write(DMA + 0x70, 1);
                write(DMA + 0x70, 0);
            }
            wait(DMA + 0x80, 16, 16, timer, "RX reset").expect("RX DMA not parked");
        }
        self.active = false;
        Ok(())
    }
}

/// Publish an exclusively CPU-owned, 64B-isolated region before activating RX.
/// # Safety
/// No DMA may access it; address/length must describe the caller's allocation.
pub unsafe fn publish_region(
    address: usize,
    length: usize,
    timer: &mut Timer,
) -> Result<(), Error> {
    if !super::super::memory::valid_ahb_range(address, length) {
        return Err(Error::InvalidBuffer);
    }
    sync(address, length as u32, timer)
}
