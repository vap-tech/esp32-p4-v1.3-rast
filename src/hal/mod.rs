//! Register-level HAL for ESP32-P4 rev 1.3. Peripheral drivers own core0;
//! HP1 has a fixed-role worker with its own memory and software interrupts.
pub mod clock;
pub mod csi;
pub mod dsi;
pub mod ethernet;
pub mod flash;
pub mod gpio;
pub mod i2c;
pub mod i2s;
pub mod interrupt;
pub mod jpeg;
pub mod ledc;
#[cfg(target_arch = "riscv32")]
pub mod lp;
pub mod memory;
pub mod multicore;
pub mod pie;
pub mod psram;
pub mod sdmmc;
pub mod time;
pub mod usb_serial;
pub mod watchdog;

pub mod arbitration;

#[cfg(all(
    target_arch = "riscv32",
    any(feature = "jpeg-lab", feature = "ppa-lab")
))]
pub(crate) mod dma2d;
pub mod ppa;
