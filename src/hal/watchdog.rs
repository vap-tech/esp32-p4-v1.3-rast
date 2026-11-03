//! Watchdogs inherited from ROM on ESP32-P4 rev 1.x.

/// Disable RTC/MWDT flashboot protection and the super-watchdog.
///
/// # Safety
/// Call during single-core startup with interrupts disabled, before using timers.
pub unsafe fn disable() {
    unsafe {
        for base in [0x5011_6000usize, 0x500c_2000, 0x500c_3000] {
            let (config_offset, protect_offset) = if base == 0x5011_6000 {
                (0, 0x18)
            } else {
                (0x48, 0x64)
            };
            let config = (base + config_offset) as *mut u32;
            let protect = (base + protect_offset) as *mut u32;
            protect.write_volatile(0x50d8_3aa1);
            let flashboot_bit = if base == 0x5011_6000 { 12 } else { 14 };
            let value = config.read_volatile() & !((1 << 31) | (1 << flashboot_bit));
            // Timer-group changes cross clock domains and need UPDATE_EN.
            let update = if base == 0x5011_6000 { 0 } else { 1 << 22 };
            config.write_volatile(value | update);
            protect.write_volatile(0);
        }
        // Disable the super-watchdog.
        let swd_config = 0x5011_601c as *mut u32;
        let swd_protect = 0x5011_6020 as *mut u32;
        swd_protect.write_volatile(0x50d8_3aa1);
        swd_config.write_volatile(swd_config.read_volatile() | (1 << 30));
        swd_protect.write_volatile(0);
    }
}
