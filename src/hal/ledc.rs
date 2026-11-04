// SPDX-FileCopyrightText: 2023-2026 Espressif Systems (Shanghai) CO LTD
// SPDX-License-Identifier: Apache-2.0
// Register sequences adapted from ESP-IDF v6.1 ledc_ll.h / ledc.c, hw_ver1.
//! One LEDC output: channel 1, timer 1, XTAL 40 MHz, 20 kHz, 10-bit duty.
use super::gpio::Output;

const BASE: usize = 0x500d_3000;
const CLOCK: usize = 0x500e_6000;
const TIMER: usize = BASE + 0xa8;
const CHANNEL: usize = BASE + 0x14;
const MAX_DUTY: u16 = 1024;
const DIVIDER_Q8: u32 = 500; // 40_000_000 * 256 / (20_000 * 1024).

pub struct Pwm<const PIN: u8> {
    pin: Output<PIN>,
}

impl<const PIN: u8> Pwm<PIN> {
    /// Consume an already configured GPIO output; start with duty zero.
    ///
    /// # Safety
    /// Own the whole LEDC peripheral exclusively: this resets it and selects
    /// its shared clock. Requires ESP32-P4 rev 1.x and a 40-MHz crystal.
    /// No concurrent clock/matrix changes. The pin must support active-high PWM.
    pub unsafe fn new(mut pin: Output<PIN>) -> Self {
        pin.set_low();
        modify(CLOCK + 0x20, 0, 1); // LEDC APB clock.
        modify(CLOCK + 0xc4, 0, 1 << 29);
        modify(CLOCK + 0xc4, 1 << 29, 0);
        modify(CLOCK + 0x9c, 3, 1 << 2); // XTAL source = 0, functional clock on.
        write(BASE + 0xc8, 0); // No interrupts.
        write(TIMER, 10 | (DIVIDER_Q8 << 5) | (1 << 24));
        modify(TIMER, 0, 1 << 26); // Apply resolution/divider.
        modify(TIMER, 1 << 24, 0); // Release timer reset.
        write(CHANNEL, 1); // Timer 1, output disabled, idle low.
        write(CHANNEL + 4, 0); // HPOINT = 0.
                               // Clear all gamma entries: uninitialized RAM can affect output.
        for entry in 0..16 {
            write(BASE + 0x440 + entry * 4, 0);
        }
        // One cycle, one step, zero scale: load duty without hardware fade.
        write(BASE + 0x440, (1 << 21) | (1 << 1) | 1);
        write(BASE + 0x104, 1); // One gamma range.
        let mut pwm = Self { pin };
        pwm.set_duty(0);
        // Matrix signal 127 = LEDC low-speed channel 1. GPIO_ENABLE retained.
        write(0x500e_0558 + usize::from(PIN) * 4, 127 | (1 << 10));
        pwm
    }

    /// Linear PWM duty in percent, saturating at 100. Not perceptual brightness.
    /// The new duty is applied asynchronously at the next PWM cycles.
    pub fn set_brightness(&mut self, percent: u8) {
        self.set_duty(((u32::from(percent.min(100)) * 1024 + 50) / 100) as u16);
    }

    /// Raw duty 0..=1024, saturating. 0 is off; 1024 is constant high (100%).
    pub fn set_duty(&mut self, duty: u16) {
        write(CHANNEL + 8, u32::from(duty.min(MAX_DUTY)) << 4);
        write(CHANNEL + 12, 1 << 31); // DUTY_START.
        write(CHANNEL, 1 | (1 << 2) | (1 << 4)); // Timer 1, enable, PARA_UP.
    }

    /// Actual duty read from hardware; may lag a just-issued update.
    pub fn duty(&self) -> u16 {
        ((read(CHANNEL + 16) & 0x01ff_ffff) >> 4) as u16
    }

    /// Read the physical pad through GPIO_IN, for waveform diagnostics.
    pub fn level(&self) -> bool {
        self.pin.level()
    }
}

fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}
fn write(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) };
}
fn modify(address: usize, clear: u32, set: u32) {
    write(address, (read(address) & !clear) | set);
}
