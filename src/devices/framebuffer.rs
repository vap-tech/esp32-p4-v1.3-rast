//! One full RGB565 display framebuffer, owned PSRAM, uncached CPU accesses.
use crate::hal::psram::{DisplayMemory, BASE};

pub const WIDTH: usize = 1024;
pub const HEIGHT: usize = 600;
pub const BYTES: usize = WIDTH * HEIGHT * 2;
pub const DMA_ADDRESS: usize = 0x4800_0080;
pub const CPU_ADDRESS: usize = BASE + 128;

pub struct Framebuffer {
    _memory: DisplayMemory,
}
impl Framebuffer {
    pub fn new(memory: DisplayMemory) -> Self {
        let mut this = Self::guarded(memory);
        this.color_bars();
        this
    }
    /// Initialize a uniform frame directly, without drawing intermediate test bars.
    pub fn new_filled(memory: DisplayMemory, color: u16) -> Self {
        let mut this = Self::guarded(memory);
        this.fill_rgb565(color);
        this
    }
    fn guarded(memory: DisplayMemory) -> Self {
        let this = Self { _memory: memory };
        unsafe {
            for i in 0..32 {
                (BASE as *mut u32).add(i).write_volatile(0x5aa5_5aa5);
                ((CPU_ADDRESS + BYTES) as *mut u32)
                    .add(i)
                    .write_volatile(0x5aa5_5aa5);
            }
        }
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
    /// Write one bounded RGB565 run (e.g. an MCU row from the JPEG reader).
    pub fn write_run(&mut self, x: usize, y: usize, pixels: &[u16]) {
        assert!(y < HEIGHT && x <= WIDTH && pixels.len() <= WIDTH - x);
        let start = y * WIDTH + x;
        // Pack adjacent RGB565 pixels, pacing external writes for LCD reads.
        unsafe {
            write_pixels((CPU_ADDRESS as *mut u16).add(start), pixels, || {
                core::arch::asm!("fence rw, rw", options(nostack));
                // Board boot policy explicitly sets CPU to 160MHz: 320 cycles=2us.
                let start = crate::hal::time::cycle_count();
                while crate::hal::time::cycle_count().wrapping_sub(start) < 320 {
                    core::hint::spin_loop();
                }
            });
        }
    }

    pub fn finish_writes(&mut self) {
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
    /// Read-only FNV-1a over RGB565 words, for memory-isolation diagnostics.
    pub fn checksum(&self) -> u32 {
        let mut hash = 0x811c9dc5u32;
        for i in 0..BYTES / 4 {
            let word = unsafe { (CPU_ADDRESS as *const u32).add(i).read_volatile() };
            hash = (hash ^ word).wrapping_mul(0x01000193);
        }
        hash
    }
    pub fn sample(&self) -> [u16; 8] {
        core::array::from_fn(|i| unsafe {
            (CPU_ADDRESS as *const u16)
                .add(WIDTH * (HEIGHT / 2) + i * WIDTH / 8 + 64)
                .read_volatile()
        })
    }
}

/// # Safety
/// Destination must own pixels.len() writable u16 elements; no overlapping source.
/// A u32 pair is written only when its address is four-byte aligned.
unsafe fn write_pixels(destination: *mut u16, pixels: &[u16], mut yield_bus: impl FnMut()) {
    let mut offset = 0;
    if destination.addr() & 3 != 0 && !pixels.is_empty() {
        unsafe {
            destination.write_volatile(pixels[0]);
        }
        offset = 1;
    }
    let (pairs, tail) = pixels[offset..].as_chunks::<2>();
    for (index, pair) in pairs.iter().enumerate() {
        let packed = u32::from(pair[0]) | (u32::from(pair[1]) << 16);
        unsafe {
            destination.add(offset).cast::<u32>().write_volatile(packed);
        }
        offset += 2;
        if (index + 1).is_multiple_of(16) {
            yield_bus();
        }
    }
    if let Some(&pixel) = tail.first() {
        unsafe {
            destination.add(offset).write_volatile(pixel);
        }
    }
}
#[cfg(test)]
mod packed_tests {
    #[test]
    fn rgb565_pairs_preserve_order_alignment_and_guards() {
        for start in [0, 1] {
            for len in 0..=34 {
                let mut backing = [0x7c1f7c1fu32; 20];
                let values: [u16; 34] =
                    core::array::from_fn(|i| (i as u16).wrapping_mul(257).wrapping_add(0x1234));
                let pointer = backing.as_mut_ptr().cast::<u16>();
                unsafe {
                    super::write_pixels(pointer.add(start), &values[..len], || {});
                }
                for i in 0..40 {
                    let actual = unsafe { pointer.add(i).read_volatile() };
                    let expected = if (start..start + len).contains(&i) {
                        values[i - start]
                    } else {
                        0x7c1f
                    };
                    assert_eq!(actual, expected, "start={} len={} index={}", start, len, i);
                }
            }
        }
    }
}
