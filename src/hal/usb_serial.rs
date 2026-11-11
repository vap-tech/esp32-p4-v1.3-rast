//! USB Serial/JTAG console; inherits the PHY setup from ROM.
use core::{fmt, marker::PhantomData};

const FIFO: *mut u32 = 0x500d_2000 as *mut u32;
const CONFIG: *mut u32 = 0x500d_2004 as *mut u32;

/// Exclusive console with bounded USB backpressure. No allocator, buffering, or USB stack.
pub struct Console {
    _exclusive: PhantomData<*mut ()>,
}

impl Console {
    /// # Safety
    /// Caller must exclusively own USB Serial/JTAG and keep its clock/PHY enabled.
    pub unsafe fn new() -> Self {
        Self {
            _exclusive: PhantomData,
        }
    }

    fn put(&mut self, byte: u8) -> bool {
        unsafe {
            // A battery-powered boot must not wait for a USB host.
            let mut budget = 10_000;
            while CONFIG.read_volatile() & 2 == 0 {
                if budget == 0 {
                    return false;
                }
                budget -= 1;
                core::hint::spin_loop();
            }
            FIFO.write_volatile(byte as u32);
        }
        true
    }

    fn flush(&mut self) {
        unsafe { CONFIG.write_volatile(1) };
    }

    /// Read one host byte if available; never blocks.
    pub fn read_byte(&mut self) -> Option<u8> {
        unsafe {
            if CONFIG.read_volatile() & (1 << 2) != 0 {
                Some(FIFO.read_volatile() as u8)
            } else {
                None
            }
        }
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' && !self.put(b'\r') {
                return Ok(());
            }
            if !self.put(byte) {
                return Ok(());
            }
        }
        // Submit even a short packet. Longer writes are sent when FIFO fills.
        self.flush();
        Ok(())
    }
}
