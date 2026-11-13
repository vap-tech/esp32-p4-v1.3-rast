//! Small, register-level HAL for ESP32-P4 rev 1.3. Single core; scoped SDMMC/timer interrupts.
pub mod clock;
pub mod csi;
pub mod dsi;
pub mod ethernet;
pub mod flash;
pub mod gpio;
pub mod i2c;
pub mod i2s;
pub mod interrupt;
pub mod ledc;
pub mod memory;
pub mod psram;
pub mod sdmmc;
pub mod time;
pub mod usb_serial;
pub mod watchdog;

pub mod arbitration;
