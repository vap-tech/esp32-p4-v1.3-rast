//! Push-pull GPIO output for the low bank (GPIO0..31), through the GPIO matrix.
use core::marker::PhantomData;

const GPIO: usize = 0x500e_0000;
const IOMUX: usize = 0x500e_1000;

pub struct Output<const PIN: u8> {
    _exclusive: PhantomData<*mut ()>,
}

impl<const PIN: u8> Output<PIN> {
    /// Configure an output, initially low, with input readback enabled.
    ///
    /// # Safety
    /// Caller must own this pad exclusively and ensure it is available on the
    /// board. Supports GPIO0..31, excluding USB pins 24..26. GPIO27 is usable
    /// only when USB FS OTG is disabled (this board uses it for LCD RESET).
    /// USB Serial/JTAG uses GPIO24/25 and is unaffected. Clocks must not
    /// be reconfigured concurrently. Does not configure power domains or holds.
    pub unsafe fn new_low() -> Self {
        assert!(PIN < 32 && !(PIN >= 24 && PIN <= 26));
        let mut output = Self {
            _exclusive: PhantomData,
        };
        unsafe {
            // IOMUX APB and functional clocks. Preserve unrelated peripherals.
            let apb = 0x500e_6020 as *mut u32;
            apb.write_volatile(apb.read_volatile() | (1 << 3));
            let clock = 0x500e_60ac as *mut u32;
            clock.write_volatile(clock.read_volatile() | (1 << 9));
            // Disable output while changing routing; preload the output latch.
            ((GPIO + 0x28) as *mut u32).write_volatile(1 << PIN);
            output.set_low();
            let pin = (GPIO + 0x74 + usize::from(PIN) * 4) as *mut u32;
            pin.write_volatile(pin.read_volatile() & !(1 << 2)); // Push-pull.
            let matrix = (GPIO + 0x558 + usize::from(PIN) * 4) as *mut u32;
            matrix.write_volatile(256 | (1 << 10)); // GPIO_OUT + GPIO_ENABLE.
            let mux = (IOMUX + 4 + usize::from(PIN) * 4) as *mut u32;
            let clear = (7 << 12) | (1 << 1) | (1 << 7) | (1 << 8);
            mux.write_volatile((mux.read_volatile() & !clear) | (1 << 12) | (1 << 9));
            ((GPIO + 0x24) as *mut u32).write_volatile(1 << PIN);
        }
        output
    }

    pub fn set_high(&mut self) {
        unsafe { ((GPIO + 8) as *mut u32).write_volatile(1 << PIN) };
    }

    pub fn set_low(&mut self) {
        unsafe { ((GPIO + 12) as *mut u32).write_volatile(1 << PIN) };
    }

    /// Sample GPIO_IN (the pad), rather than the commanded output latch.
    pub fn level(&self) -> bool {
        unsafe { ((GPIO + 0x3c) as *const u32).read_volatile() & (1 << PIN) != 0 }
    }
}
