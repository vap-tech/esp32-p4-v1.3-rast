//! JC1060P470C display: panel commands, DSI patterns, reset and backlight.
//! Register semantics: JADARD JD9165A-A Rev.0.0.1, checked on the board's BA panel.
//! No heap/runtime dependency. Optional PSRAM framebuffer scanout on DMA CH2.
use crate::hal::{
    dsi::{Dsi, Error as DsiError, LaneRate, Pattern, PixelFormat},
    gpio::Output,
    ledc::Pwm,
    time::Timer,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Dsi(DsiError),
    Asleep,
    InvalidScanline,
}
impl From<DsiError> for Error {
    fn from(error: DsiError) -> Self {
        Self::Dsi(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorOrder {
    Rgb,
    Bgr,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reset {
    Hardware,
    Software,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    Black,
    White,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TearEffect {
    Off,
    Vblank,
    VblankAndHblank,
}

#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub id: [u8; 3],
    /// DCS 09h bytes in wire order; preliminary manuals disagree on bit layout.
    pub display: [u8; 4],
    pub power: u8,
    pub address: u8,
    pub image: u8,
    pub signal: u8,
    pub self_diagnostic: u8,
}
impl Snapshot {
    pub fn awake(&self) -> bool {
        self.power & 0x10 != 0
    }
    pub fn display_on(&self) -> bool {
        self.power & 4 != 0
    }
    pub fn inverted(&self) -> bool {
        self.image & 0x20 != 0
    }
    /// BA panel readback: idle sets power bit 6.
    pub fn idle(&self) -> bool {
        self.power & 0x40 != 0
    }
    pub fn mirror(&self) -> (bool, bool) {
        (self.address & 2 != 0, self.address & 1 != 0)
    }
    /// BA panel readback: RGB/BGR is status byte 0 bit 2.
    pub fn color_order(&self) -> ColorOrder {
        if self.display[0] & 4 != 0 {
            ColorOrder::Bgr
        } else {
            ColorOrder::Rgb
        }
    }
    pub fn tear_effect(&self) -> TearEffect {
        if self.signal & 0x80 == 0 {
            TearEffect::Off
        } else if self.signal & 0x40 == 0 {
            TearEffect::Vblank
        } else {
            TearEffect::VblankAndHblank
        }
    }
}

/// DCS 05h counts packet/parity errors, not erroneous payload bits.
/// Reading it clears the panel counter; count 127 + overflow is not an exact total.
#[derive(Clone, Copy, Debug)]
pub struct PacketErrors {
    pub count: u8,
    pub overflow: bool,
    pub raw: u8,
}

/// Result of a bounded experiment, not proof that the panel entered BIST.
#[derive(Clone, Copy, Debug)]
pub struct BistProbe {
    pub host_during_probe: crate::hal::dsi::Status,
    pub observation_us: u64,
}

/// Owns the board's DSI host, LCD RESET and PWM backlight.
pub struct Display {
    dsi: Dsi,
    reset: Output<27>,
    backlight: Pwm<23>,
    pattern: Pattern,
    brightness: u8,
    address: u8,
    inverted: bool,
    idle: bool,
    enabled: bool,
    asleep: bool,
    tear: TearEffect,
    tear_line: Option<u16>,
    idle_color: u8,
}

impl Display {
    pub const WIDTH: u16 = 1024;
    pub const HEIGHT: u16 = 600;

    pub fn new(
        dsi: Dsi,
        reset: Output<27>,
        backlight: Pwm<23>,
        timer: &mut Timer,
    ) -> Result<Self, Error> {
        let mut display = Self {
            dsi,
            reset,
            backlight,
            pattern: Pattern::Vertical,
            brightness: 25,
            address: 0,
            inverted: false,
            idle: false,
            enabled: true,
            asleep: true,
            tear: TearEffect::Off,
            tear_line: None,
            idle_color: 0,
        };
        display.reset(Reset::Hardware, timer)?;
        Ok(display)
    }

    /// Reset/reinitialize the panel, preserving requested user settings.
    /// On failure backlight stays off; retry reset to recover unknown panel state.
    pub fn reset(&mut self, kind: Reset, timer: &mut Timer) -> Result<(), Error> {
        self.backlight.set_brightness(0);
        self.asleep = true;
        self.dsi.set_video_enabled(false);
        match kind {
            Reset::Hardware => {
                init(&mut self.dsi, &mut self.reset, timer)?;
            }
            Reset::Software => {
                self.dsi.write_dcs(0x30, &[0], timer)?;
                self.dsi.write_dcs(0x01, &[], timer)?;
                timer.delay_us(120_000);
                configure(&mut self.dsi, timer)?;
            }
        }
        self.restore_settings(timer)?;
        // Stay in command mode until an explicit framebuffer/pattern start.
        self.asleep = false;
        self.backlight.set_brightness(self.brightness);
        timer.delay_us(100);
        Ok(())
    }

    fn awake(&self) -> Result<(), Error> {
        if self.asleep {
            Err(Error::Asleep)
        } else {
            Ok(())
        }
    }

    fn restore_settings(&mut self, timer: &mut Timer) -> Result<(), Error> {
        self.dsi.write_dcs(0x36, &[self.address], timer)?;
        self.dsi
            .write_dcs(if self.inverted { 0x21 } else { 0x20 }, &[], timer)?;
        self.dsi
            .write_dcs(if self.idle { 0x39 } else { 0x38 }, &[], timer)?;
        self.dsi.write_dcs(0x80, &[self.idle_color], timer)?;
        self.write_tear(TearEffect::Off, timer)?;
        if let Some(line) = self.tear_line {
            self.dsi.write_dcs(0x44, &line.to_be_bytes(), timer)?;
            self.write_tear(TearEffect::Off, timer)?;
        }
        self.write_tear(self.tear, timer)?;
        self.dsi
            .write_dcs(if self.enabled { 0x29 } else { 0x28 }, &[], timer)?;
        timer.delay_us(40_000);
        Ok(())
    }

    pub fn set_brightness(&mut self, percent: u8) {
        self.brightness = percent.min(100);
        if !self.asleep {
            self.backlight.set_brightness(self.brightness);
        }
    }
    pub fn brightness(&self) -> u8 {
        self.brightness
    }
    pub fn pwm_duty(&self) -> u16 {
        self.backlight.duty()
    }
    /// # Safety
    /// Keep this PSRAM framebuffer alive until stop_framebuffer; own DMA CH2.
    pub unsafe fn start_framebuffer(
        &mut self,
        address: usize,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        self.awake()?;
        unsafe {
            self.dsi.start_framebuffer(address, timer)?;
        }
        Ok(())
    }
    pub fn framebuffer_status(&mut self) -> [u32; 12] {
        self.dsi.framebuffer_status()
    }
    pub fn stop_framebuffer(&mut self) {
        self.dsi.stop_framebuffer();
        self.dsi.start_pattern(self.pattern);
    }

    pub fn host_status(&mut self) -> crate::hal::dsi::Status {
        self.dsi.diagnostics()
    }

    /// Experimental vendor-page pair from a first-hand JD9165 bring-up report.
    /// Stops host video, writes page1 05=01/06=40, observes for 25 seconds, then
    /// always attempts hardware reset + normal init to restore user settings.
    /// No USB writes occur while armed, so console backpressure cannot prevent
    /// timed restoration. The caller must visually verify the panel output.
    pub fn probe_panel_bist(&mut self, timer: &mut Timer) -> Result<BistProbe, Error> {
        self.awake()?;
        let result = (|| {
            self.dsi.set_video_enabled(false);
            self.dsi.write_dcs(0x30, &[1], timer)?;
            self.dsi.write_dcs(0x05, &[1], timer)?;
            self.dsi.write_dcs(0x06, &[0x40], timer)?;
            self.dsi.write_dcs(0x30, &[0], timer)?;
            timer.delay_us(1_000);
            let host_during_probe = self.dsi.diagnostics();
            let start = timer.now_us();
            timer.delay_us(25_000_000);
            Ok(BistProbe {
                host_during_probe,
                observation_us: timer.now_us().wrapping_sub(start) & ((1 << 48) - 1),
            })
        })();
        // Recover even if an experimental write failed on a nonzero page.
        let _ = self.dsi.write_dcs(0x30, &[0], timer);
        self.reset(Reset::Hardware, timer)?;
        result
    }

    pub fn set_pattern(&mut self, pattern: Pattern) -> Result<(), Error> {
        self.awake()?;
        self.dsi.start_pattern(pattern);
        self.pattern = pattern;
        Ok(())
    }

    /// Configure the host's diagnostic video stream; no framebuffer involved.
    pub fn set_video_config(
        &mut self,
        rate: LaneRate,
        format: PixelFormat,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        self.awake()?;
        self.dsi.set_video_config(rate, format, timer)?;
        Ok(())
    }

    pub fn set_display_on(&mut self, enabled: bool, timer: &mut Timer) -> Result<(), Error> {
        self.awake()?;
        self.dsi
            .write_dcs(if enabled { 0x29 } else { 0x28 }, &[], timer)?;
        timer.delay_us(40_000);
        self.enabled = enabled;
        Ok(())
    }
    pub fn set_inverted(&mut self, enabled: bool, timer: &mut Timer) -> Result<(), Error> {
        self.awake()?;
        self.dsi
            .write_dcs(if enabled { 0x21 } else { 0x20 }, &[], timer)?;
        self.inverted = enabled;
        Ok(())
    }
    pub fn set_idle(&mut self, enabled: bool, timer: &mut Timer) -> Result<(), Error> {
        self.awake()?;
        self.dsi
            .write_dcs(if enabled { 0x39 } else { 0x38 }, &[], timer)?;
        self.idle = enabled;
        Ok(())
    }
    pub fn set_mirror(
        &mut self,
        horizontal: bool,
        vertical: bool,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        self.set_address(
            (self.address & 8) | (u8::from(horizontal) << 1) | u8::from(vertical),
            timer,
        )
    }
    pub fn set_color_order(&mut self, order: ColorOrder, timer: &mut Timer) -> Result<(), Error> {
        self.set_address(
            (self.address & 3) | if order == ColorOrder::Bgr { 8 } else { 0 },
            timer,
        )
    }
    fn set_address(&mut self, value: u8, timer: &mut Timer) -> Result<(), Error> {
        self.awake()?;
        self.dsi.write_dcs(0x36, &[value], timer)?;
        self.address = value;
        Ok(())
    }

    /// Black/white pixel override. None restores normal pixels via reinitialization:
    /// NORON 13h is referenced but missing from the available command table.
    pub fn set_test_fill(&mut self, fill: Option<Fill>, timer: &mut Timer) -> Result<(), Error> {
        self.awake()?;
        match fill {
            Some(fill) => {
                self.dsi
                    .write_dcs(if fill == Fill::Black { 0x22 } else { 0x23 }, &[], timer)?
            }
            None => self.reset(Reset::Hardware, timer)?,
        }
        Ok(())
    }

    fn write_tear(&mut self, mode: TearEffect, timer: &mut Timer) -> Result<(), Error> {
        match mode {
            TearEffect::Off => self.dsi.write_dcs(0x34, &[], timer)?,
            TearEffect::Vblank => self.dsi.write_dcs(0x35, &[0], timer)?,
            TearEffect::VblankAndHblank => self.dsi.write_dcs(0x35, &[1], timer)?,
        }
        Ok(())
    }
    /// Configures the panel output only; no TE pin is identified on this PCB.
    pub fn set_tear_effect(&mut self, mode: TearEffect, timer: &mut Timer) -> Result<(), Error> {
        self.awake()?;
        // TEON may ignore mode changes while already on, so turn it off first.
        self.write_tear(TearEffect::Off, timer)?;
        self.dsi.write_dcs(0x44, &[0, 0], timer)?;
        self.write_tear(TearEffect::Off, timer)?;
        self.write_tear(mode, timer)?;
        self.tear = mode;
        self.tear_line = None;
        Ok(())
    }
    pub fn set_tear_scanline(&mut self, line: u16, timer: &mut Timer) -> Result<(), Error> {
        self.awake()?;
        if line >= Self::HEIGHT {
            return Err(Error::InvalidScanline);
        }
        self.write_tear(TearEffect::Off, timer)?;
        self.dsi.write_dcs(0x44, &line.to_be_bytes(), timer)?;
        self.write_tear(TearEffect::Off, timer)?;
        self.write_tear(TearEffect::Vblank, timer)?;
        self.tear = TearEffect::Vblank;
        self.tear_line = Some(line);
        Ok(())
    }
    /// Idle mode color selection (bits R,G,B), not a framebuffer pixel format.
    pub fn set_idle_color(
        &mut self,
        red: bool,
        green: bool,
        blue: bool,
        timer: &mut Timer,
    ) -> Result<(), Error> {
        self.awake()?;
        let value = (u8::from(red) << 2) | (u8::from(green) << 1) | u8::from(blue);
        self.dsi.write_dcs(0x80, &[value], timer)?;
        self.idle_color = value;
        Ok(())
    }
    pub fn idle_color(&mut self, timer: &mut Timer) -> Result<u8, Error> {
        self.read_byte(0x81, timer)
    }

    /// Safe power-down sequence: backlight off, DISPOFF, SLPIN, two video frames,
    /// then stop host video. Preserve requested brightness/display settings.
    pub fn sleep(&mut self, timer: &mut Timer) -> Result<(), Error> {
        if self.asleep {
            return Ok(());
        }
        self.backlight.set_brightness(0);
        self.dsi.write_dcs(0x28, &[], timer)?;
        timer.delay_us(40_000);
        self.dsi.write_dcs(0x10, &[], timer)?;
        timer.delay_us(40_000);
        self.dsi.set_video_enabled(false);
        timer.delay_us(80_000);
        self.asleep = true;
        Ok(())
    }
    pub fn wake(&mut self, timer: &mut Timer) -> Result<(), Error> {
        if !self.asleep {
            return Ok(());
        }
        self.dsi.set_video_enabled(true);
        timer.delay_us(20_000);
        self.dsi.write_dcs(0x11, &[], timer)?;
        timer.delay_us(120_000);
        self.restore_settings(timer)?;
        self.asleep = false;
        self.backlight.set_brightness(self.brightness);
        timer.delay_us(100);
        Ok(())
    }

    fn read_byte(&mut self, command: u8, timer: &mut Timer) -> Result<u8, Error> {
        Ok(self.dsi.read_dcs(command, 1, timer)? as u8)
    }
    /// Reads use LP/BTA in vertical blanking. Does not clear DCS 05h counter.
    pub fn snapshot(&mut self, timer: &mut Timer) -> Result<Snapshot, Error> {
        let id = self.dsi.read_dcs(0x04, 3, timer)?.to_le_bytes();
        Ok(Snapshot {
            id: [id[0], id[1], id[2]],
            display: self.dsi.read_dcs(0x09, 4, timer)?.to_le_bytes(),
            power: self.read_byte(0x0a, timer)?,
            address: self.read_byte(0x0b, timer)?,
            image: self.read_byte(0x0d, timer)?,
            signal: self.read_byte(0x0e, timer)?,
            self_diagnostic: self.read_byte(0x0f, timer)?,
        })
    }
    pub fn take_packet_errors(&mut self, timer: &mut Timer) -> Result<PacketErrors, Error> {
        let raw = self.read_byte(0x05, timer)?;
        Ok(PacketErrors {
            count: raw & 0x7f,
            overflow: raw & 0x80 != 0,
            raw,
        })
    }
    pub fn first_pixel(&mut self, timer: &mut Timer) -> Result<[u8; 3], Error> {
        Ok([
            self.read_byte(0x06, timer)?,
            self.read_byte(0x07, timer)?,
            self.read_byte(0x08, timer)?,
        ])
    }
    /// Probe scanline support. BA panel may time out; not a VSYNC timestamp.
    pub fn scanline(&mut self, timer: &mut Timer) -> Result<u16, Error> {
        let bytes = self.dsi.read_dcs(0x45, 2, timer)?.to_le_bytes();
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }
}

pub fn init(dsi: &mut Dsi, reset: &mut Output<27>, timer: &mut Timer) -> Result<[u8; 3], Error> {
    reset.set_high();
    timer.delay_us(10_000);
    reset.set_low();
    timer.delay_us(10_000);
    reset.set_high();
    timer.delay_us(120_000);
    configure(dsi, timer)
}
fn configure(dsi: &mut Dsi, timer: &mut Timer) -> Result<[u8; 3], Error> {
    // Explicitly restore the base page, including after experimental commands.
    dsi.write_dcs(0x30, &[0], timer)?;
    let id = dsi.read_dcs(0x04, 3, timer)?.to_le_bytes();
    dsi.write_dcs(0x36, &[0], timer)?;
    for &(command, data, delay_ms) in INIT {
        dsi.write_dcs(command, data, timer)?;
        timer.delay_us(delay_ms * 1000);
    }
    dsi.write_dcs(0x29, &[], timer)?;
    timer.delay_us(20_000);
    Ok([id[0], id[1], id[2]])
}

const INIT: &[(u8, &[u8], u32)] = &[
    (0x30, &[0x00], 0),
    (0xF7, &[0x49, 0x61, 0x02, 0x00], 0),
    (0x30, &[0x01], 0),
    (0x04, &[0x0C], 0),
    (0x05, &[0x00], 0),
    (0x06, &[0x00], 0),
    (0x0B, &[0x11], 0),
    (0x17, &[0x00], 0),
    (0x20, &[0x04], 0),
    (0x1F, &[0x05], 0),
    (0x23, &[0x00], 0),
    (0x25, &[0x19], 0),
    (0x28, &[0x18], 0),
    (0x29, &[0x04], 0),
    (0x2A, &[0x01], 0),
    (0x2B, &[0x04], 0),
    (0x2C, &[0x01], 0),
    (0x30, &[0x02], 0),
    (0x01, &[0x22], 0),
    (0x03, &[0x12], 0),
    (0x04, &[0x00], 0),
    (0x05, &[0x64], 0),
    (0x0A, &[0x08], 0),
    (
        0x0B,
        &[
            0x0A, 0x1A, 0x0B, 0x0D, 0x0D, 0x11, 0x10, 0x06, 0x08, 0x1F, 0x1D,
        ],
        0,
    ),
    (
        0x0C,
        &[
            0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D,
        ],
        0,
    ),
    (
        0x0D,
        &[
            0x16, 0x1B, 0x0B, 0x0D, 0x0D, 0x11, 0x10, 0x07, 0x09, 0x1E, 0x1C,
        ],
        0,
    ),
    (
        0x0E,
        &[
            0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D,
        ],
        0,
    ),
    (
        0x0F,
        &[
            0x16, 0x1B, 0x0D, 0x0B, 0x0D, 0x11, 0x10, 0x1C, 0x1E, 0x09, 0x07,
        ],
        0,
    ),
    (
        0x10,
        &[
            0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D,
        ],
        0,
    ),
    (
        0x11,
        &[
            0x0A, 0x1A, 0x0D, 0x0B, 0x0D, 0x11, 0x10, 0x1D, 0x1F, 0x08, 0x06,
        ],
        0,
    ),
    (
        0x12,
        &[
            0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D, 0x0D,
        ],
        0,
    ),
    (0x14, &[0x00, 0x00, 0x11, 0x11], 0),
    (0x18, &[0x99], 0),
    (0x30, &[0x06], 0),
    (
        0x12,
        &[
            0x36, 0x2C, 0x2E, 0x3C, 0x38, 0x35, 0x35, 0x32, 0x2E, 0x1D, 0x2B, 0x21, 0x16, 0x29,
        ],
        0,
    ),
    (
        0x13,
        &[
            0x36, 0x2C, 0x2E, 0x3C, 0x38, 0x35, 0x35, 0x32, 0x2E, 0x1D, 0x2B, 0x21, 0x16, 0x29,
        ],
        0,
    ),
    (0x30, &[0x0A], 0),
    (0x02, &[0x4F], 0),
    (0x0B, &[0x40], 0),
    (0x12, &[0x3E], 0),
    (0x13, &[0x78], 0),
    (0x30, &[0x0D], 0),
    (0x0D, &[0x04], 0),
    (0x10, &[0x0C], 0),
    (0x11, &[0x0C], 0),
    (0x12, &[0x0C], 0),
    (0x13, &[0x0C], 0),
    (0x30, &[0x00], 0),
    (0x11, &[0x00], 120),
    (0x29, &[0x00], 50),
];
