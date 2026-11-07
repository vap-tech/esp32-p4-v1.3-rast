//! Small, register-level HAL for ESP32-P4 rev 1.3. Single core, no interrupts.
pub mod csi;
pub mod dsi;
pub mod ethernet;
pub mod gpio;
pub mod i2c;
pub mod i2s;
pub mod ledc;
pub mod memory;
pub mod psram;
pub mod sdmmc;
pub mod time;
pub mod usb_serial;
pub mod watchdog;
