//! One full RGB565 display framebuffer, owned PSRAM, uncached CPU accesses.
use crate::hal::psram::{Psram, BASE};

pub const WIDTH: usize = 1024;
pub const HEIGHT: usize = 600;
pub const BYTES: usize = WIDTH * HEIGHT * 2;
pub const DMA_ADDRESS: usize = 0x4800_0080;
pub const CPU_ADDRESS: usize = BASE + 128;

pub struct Framebuffer {
    _memory: Psram,
}
impl Framebuffer {
    pub fn new(memory: Psram) -> Self {
        let mut this = Self { _memory: memory };
        unsafe {
            for i in 0..32 {
                (BASE as *mut u32).add(i).write_volatile(0x5aa5_5aa5);
                ((CPU_ADDRESS + BYTES) as *mut u32)
                    .add(i)
                    .write_volatile(0x5aa5_5aa5);
            }
        }
        this.color_bars();
        this
    }
    pub fn fill_rgb565_halfwords(&mut self, color: u16) {
        for i in 0..BYTES / 2 {
            unsafe {
                (CPU_ADDRESS as *mut u16).add(i).write_volatile(color);
            }
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
    }
    pub fn fill_rgb565(&mut self, color: u16) {
        let pair = u32::from(color) * 0x0001_0001;
        for i in 0..BYTES / 4 {
            unsafe {
                (CPU_ADDRESS as *mut u32).add(i).write_volatile(pair);
            }
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
    }
    pub fn color_bars(&mut self) {
        const COLORS: [u16; 8] = [0xffff, 0xffe0, 0x07ff, 0x07e0, 0xf81f, 0xf800, 0x001f, 0];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let color = COLORS[x / 128];
                unsafe {
                    (CPU_ADDRESS as *mut u16)
                        .add(y * WIDTH + x)
                        .write_volatile(color);
                }
            }
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
    }
    /// Top: 32 neutral gray steps. Bottom: R/G/B blocks and intensity ramps.
    pub fn color_check(&mut self) {
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let color = if y < 300 {
                    let level = (x / 32) as u16;
                    // Expand 5-bit gray to 6-bit green with matching endpoints.
                    (level << 11) | (((level << 1) | (level >> 4)) << 5) | level
                } else {
                    let band = (x * 3 / WIDTH).min(2);
                    let level = if y < 450 {
                        31
                    } else {
                        ((y - 450) * 31 / 149) as u16
                    };
                    match band {
                        0 => level << 11,
                        1 => ((level << 1) | (level >> 4)) << 5,
                        _ => level,
                    }
                };
                unsafe {
                    (CPU_ADDRESS as *mut u16)
                        .add(y * WIDTH + x)
                        .write_volatile(color);
                }
            }
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
    }
    pub fn pixel(&self, x: usize, y: usize) -> Option<u16> {
        if x >= WIDTH || y >= HEIGHT {
            return None;
        }
        Some(unsafe {
            (CPU_ADDRESS as *const u16)
                .add(y * WIDTH + x)
                .read_volatile()
        })
    }
    /// Copy to a reserved PSRAM screenshot area. Caller must stop camera DMA.
    pub fn snapshot(&mut self) {
        for i in 0..BYTES / 4 {
            unsafe {
                let word = (CPU_ADDRESS as *const u32).add(i).read_volatile();
                ((BASE + 2 * 1024 * 1024) as *mut u32)
                    .add(i)
                    .write_volatile(word);
            }
        }
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }
    }
    pub fn snapshot_pixel(&self, x: usize, y: usize) -> Option<u16> {
        if x >= WIDTH || y >= HEIGHT {
            return None;
        }
        Some(unsafe {
            ((BASE + 2 * 1024 * 1024) as *const u16)
                .add(y * WIDTH + x)
                .read_volatile()
        })
    }
    pub fn guards_ok(&self) -> bool {
        (0..32).all(|i| unsafe {
            (BASE as *const u32).add(i).read_volatile() == 0x5aa5_5aa5
                && ((CPU_ADDRESS + BYTES) as *const u32).add(i).read_volatile() == 0x5aa5_5aa5
        })
    }
    pub fn sample(&self) -> [u16; 8] {
        core::array::from_fn(|i| unsafe {
            (CPU_ADDRESS as *const u16)
                .add(WIDTH * (HEIGHT / 2) + i * WIDTH / 8 + 64)
                .read_volatile()
        })
    }
}
