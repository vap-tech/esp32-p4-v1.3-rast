//! OV02C10 SCCB control, two CSI lanes, RAW10. Experimental 64x32 crop.
// Sensor register table adapted from esp-video-components PR #46, head
// ed9f3ddc8bff764b02a0b7c22dd7179b99bdb6a3, Apache-2.0.
// SPDX-FileCopyrightText: 2024 Espressif Systems (Shanghai) CO LTD
// SPDX-License-Identifier: Apache-2.0
use crate::hal::{
    i2c::{Error, I2c},
    time::Timer,
};

pub const WIDTH: u16 = 64;
pub const HEIGHT: u16 = 32;
pub const FRAME_BYTES: usize = WIDTH as usize * HEIGHT as usize * 10 / 8;
const ADDRESS: u8 = 0x36;

#[derive(Debug)]
pub struct Snapshot {
    pub id: [u8; 3],
    pub streaming: u8,
    pub output_size: [u8; 4],
    pub pll: [u8; 5],
    pub lanes: u8,
    pub mipi: u8,
    pub exposure: [u8; 2],
    pub gain: [u8; 5],
}

pub fn read(bus: &mut I2c, reg: u16, timer: &mut Timer) -> Result<u8, Error> {
    let mut value = [0];
    bus.write_read(ADDRESS, &reg.to_be_bytes(), &mut value, timer)?;
    Ok(value[0])
}

pub fn write(bus: &mut I2c, reg: u16, value: u8, timer: &mut Timer) -> Result<(), Error> {
    bus.write_read(
        ADDRESS,
        &[(reg >> 8) as u8, reg as u8, value],
        &mut [],
        timer,
    )
}

pub fn snapshot(bus: &mut I2c, timer: &mut Timer) -> Result<Snapshot, Error> {
    Ok(Snapshot {
        id: [
            read(bus, 0x300a, timer)?,
            read(bus, 0x300b, timer)?,
            read(bus, 0x300c, timer)?,
        ],
        streaming: read(bus, 0x0100, timer)?,
        output_size: [
            read(bus, 0x3808, timer)?,
            read(bus, 0x3809, timer)?,
            read(bus, 0x380a, timer)?,
            read(bus, 0x380b, timer)?,
        ],
        pll: [
            read(bus, 0x0301, timer)?,
            read(bus, 0x0303, timer)?,
            read(bus, 0x0304, timer)?,
            read(bus, 0x0305, timer)?,
            read(bus, 0x0316, timer)?,
        ],
        lanes: read(bus, 0x3016, timer)?,
        mipi: read(bus, 0x4800, timer)?,
        exposure: [read(bus, 0x3501, timer)?, read(bus, 0x3502, timer)?],
        gain: [
            read(bus, 0x3508, timer)?,
            read(bus, 0x3509, timer)?,
            read(bus, 0x350a, timer)?,
            read(bus, 0x350b, timer)?,
            read(bus, 0x350c, timer)?,
        ],
    })
}

/// Manual camera controls. Q4 gain: 16 = 1x; Q10 gain: 1024 = 1x.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    pub exposure_lines: u16,
    pub analogue_q4: u16,
    pub digital_q10: u16,
}
impl Controls {
    pub const fn preview() -> Self {
        Self {
            exposure_lines: 1120,
            analogue_q4: 16,
            digital_q10: 1024,
        }
    }
}

pub fn read_controls(bus: &mut I2c, timer: &mut Timer) -> Result<Controls, Error> {
    let exposure_lines = u16::from_be_bytes([read(bus, 0x3501, timer)?, read(bus, 0x3502, timer)?]);
    let analogue = u16::from_be_bytes([read(bus, 0x3508, timer)?, read(bus, 0x3509, timer)?]);
    let digital = (u32::from(read(bus, 0x350a, timer)?) << 16)
        | (u32::from(read(bus, 0x350b, timer)?) << 8)
        | u32::from(read(bus, 0x350c, timer)?);
    Ok(Controls {
        exposure_lines,
        analogue_q4: analogue >> 4,
        digital_q10: (digital >> 6) as u16,
    })
}

/// Apply on a running sensor without resetting CSI/ISP. All range checks happen
/// before writes. SCCB failure may leave a partial update; read back on error.
/// Exposure is bounded by the current VTS, so this does not change frame rate.
pub fn set_controls(bus: &mut I2c, timer: &mut Timer, controls: Controls) -> Result<(), Error> {
    if !(4..=1156).contains(&controls.exposure_lines)
        || !(16..=248).contains(&controls.analogue_q4)
        || !(1024..=0x3fff).contains(&controls.digital_q10)
    {
        return Err(Error::InvalidRequest);
    }
    let vts = u16::from_be_bytes([read(bus, 0x380e, timer)?, read(bus, 0x380f, timer)?]);
    if controls.exposure_lines > vts.saturating_sub(8) {
        return Err(Error::InvalidRequest);
    }
    let exposure = controls.exposure_lines.to_be_bytes();
    let analogue = (controls.analogue_q4 << 4).to_be_bytes();
    let digital = (u32::from(controls.digital_q10) << 6).to_be_bytes();
    for (reg, value) in [
        (0x3501, exposure[0]),
        (0x3502, exposure[1]),
        (0x3508, analogue[0]),
        (0x3509, analogue[1]),
        (0x350a, digital[1]),
        (0x350b, digital[2]),
        (0x350c, digital[3]),
    ] {
        write(bus, reg, value, timer)?;
    }
    Ok(())
}

pub fn configure_crop(bus: &mut I2c, timer: &mut Timer, test_pattern: bool) -> Result<(), Error> {
    configure_window(bus, timer, WIDTH, HEIGHT, test_pattern)
}

pub fn configure_window(
    bus: &mut I2c,
    timer: &mut Timer,
    width: u16,
    height: u16,
    test_pattern: bool,
) -> Result<(), Error> {
    if width == 0 || width > 1920 || height == 0 || height > 1080 {
        return Err(Error::InvalidRequest);
    }

    // Read identity before changing sensor state. Do not program another device.
    if read(bus, 0x300a, timer)? != 0x56 || read(bus, 0x300b, timer)? != 0x02 {
        return Err(Error::InvalidRequest);
    }
    write(bus, 0x0100, 0, timer)?;
    for &(reg, value) in INIT {
        write(bus, reg, value, timer)?;
        if reg == 0x0103 {
            timer.delay_us(10_000);
        }
    }
    // Keep reference PLL and line/frame timing; shrink only the output window.
    // This is a crop of the native image, not binning/scaling or a wide preview.
    for (reg, value) in [
        (0x3808, (width >> 8) as u8),
        (0x3809, width as u8),
        (0x380a, (height >> 8) as u8),
        (0x380b, height as u8),
        (0x3501, if height > 32 { 0x04 } else { 0x01 }),
        (0x3502, if height > 32 { 0x60 } else { 0x00 }),
        // Gain control Q4 (16=1x) is shifted by 4 into the 16-bit register.
        // Valid register range is 0x0100..0x0f80; the PR's 0x4000 is invalid.
        // Unity analogue gain; preview controls apply the saved digital gain.
        (0x3508, 0x01),
        (0x3509, 0x00),
        // Digital unity: ctrl 0x0400 << 6 = 0x010000 across three registers.
        (0x350a, 0x01),
        (0x350b, 0x00),
        (0x350c, 0x00),
        (0x4800, 0x04),
        (0x4503, if test_pattern { 0x80 } else { 0 }),
    ] {
        write(bus, reg, value, timer)?;
    }
    Ok(())
}

pub fn stream(bus: &mut I2c, timer: &mut Timer, enabled: bool) -> Result<(), Error> {
    write(bus, 0x0100, u8::from(enabled), timer)
}

const INIT: &[(u16, u8)] = &[
    (0x0103, 0x01),
    (0x0301, 0x08),
    (0x0303, 0x06),
    (0x0304, 0x01),
    (0x0305, 0x90),
    (0x0313, 0x40),
    (0x031c, 0x4f),
    (0x3016, 0x32),
    (0x301b, 0xf0),
    (0x3020, 0x97),
    (0x3021, 0x23),
    (0x3022, 0x01),
    (0x3026, 0xb4),
    (0x3027, 0xf1),
    (0x303b, 0x00),
    (0x303c, 0x4f),
    (0x303d, 0xe6),
    (0x303e, 0x00),
    (0x303f, 0x03),
    (0x3501, 0x10),
    (0x3502, 0x6c),
    (0x3504, 0x0c),
    (0x3507, 0x00),
    (0x3508, 0x40),
    (0x3509, 0x00),
    (0x350a, 0x01),
    (0x350b, 0x00),
    (0x350c, 0x41),
    (0x3600, 0x84),
    (0x3603, 0x08),
    (0x3610, 0x57),
    (0x3611, 0x1b),
    (0x3613, 0x78),
    (0x3623, 0x00),
    (0x3632, 0xa0),
    (0x3642, 0xe8),
    (0x364c, 0x70),
    (0x365d, 0x00),
    (0x365f, 0x0f),
    (0x3708, 0x30),
    (0x3714, 0x24),
    (0x3725, 0x02),
    (0x3737, 0x08),
    (0x3739, 0x28),
    (0x3749, 0x32),
    (0x374a, 0x32),
    (0x374b, 0x32),
    (0x374c, 0x32),
    (0x374d, 0x81),
    (0x374e, 0x81),
    (0x374f, 0x81),
    (0x3752, 0x36),
    (0x3753, 0x36),
    (0x3754, 0x36),
    (0x3761, 0x00),
    (0x376c, 0x81),
    (0x3774, 0x18),
    (0x3776, 0x08),
    (0x377c, 0x81),
    (0x377d, 0x81),
    (0x377e, 0x81),
    (0x37a0, 0x44),
    (0x37a6, 0x44),
    (0x37aa, 0x0d),
    (0x37ae, 0x00),
    (0x37cb, 0x03),
    (0x37cc, 0x01),
    (0x37d8, 0x02),
    (0x37d9, 0x10),
    (0x37e1, 0x10),
    (0x37e2, 0x18),
    (0x37e3, 0x08),
    (0x37e4, 0x08),
    (0x37e5, 0x02),
    (0x37e6, 0x08),
    (0x3800, 0x00),
    (0x3801, 0x00),
    (0x3802, 0x00),
    (0x3803, 0x04),
    (0x3804, 0x07),
    (0x3805, 0x8f),
    (0x3806, 0x04),
    (0x3807, 0x43),
    (0x3808, 0x07),
    (0x3809, 0x80),
    (0x380a, 0x04),
    (0x380b, 0x38),
    (0x380c, 0x08),
    (0x380d, 0xe8),
    (0x380e, 0x04),
    (0x380f, 0x8c),
    (0x3810, 0x00),
    (0x3811, 0x07),
    (0x3812, 0x00),
    (0x3813, 0x04),
    (0x3814, 0x01),
    (0x3815, 0x01),
    (0x3816, 0x01),
    (0x3817, 0x01),
    (0x3820, 0xa0),
    (0x3821, 0x00),
    (0x3822, 0x80),
    (0x3823, 0x08),
    (0x3824, 0x00),
    (0x3825, 0x20),
    (0x3826, 0x00),
    (0x3827, 0x08),
    (0x382a, 0x00),
    (0x382b, 0x08),
    (0x382d, 0x00),
    (0x382e, 0x00),
    (0x382f, 0x23),
    (0x3834, 0x00),
    (0x3839, 0x00),
    (0x383a, 0xd1),
    (0x383e, 0x03),
    (0x393d, 0x29),
    (0x393f, 0x6e),
    (0x394b, 0x06),
    (0x394c, 0x06),
    (0x394d, 0x08),
    (0x394e, 0x0a),
    (0x394f, 0x01),
    (0x3950, 0x01),
    (0x3951, 0x01),
    (0x3952, 0x01),
    (0x3953, 0x01),
    (0x3954, 0x01),
    (0x3955, 0x01),
    (0x3956, 0x01),
    (0x3957, 0x0e),
    (0x3958, 0x08),
    (0x3959, 0x08),
    (0x395a, 0x08),
    (0x395b, 0x13),
    (0x395c, 0x09),
    (0x395d, 0x05),
    (0x395e, 0x02),
    (0x395f, 0x00),
    (0x3960, 0x00),
    (0x3961, 0x00),
    (0x3962, 0x00),
    (0x3963, 0x00),
    (0x3964, 0x00),
    (0x3965, 0x00),
    (0x3966, 0x00),
    (0x3967, 0x00),
    (0x3968, 0x01),
    (0x3969, 0x01),
    (0x396a, 0x01),
    (0x396b, 0x01),
    (0x396c, 0x10),
    (0x396d, 0xf0),
    (0x396e, 0x11),
    (0x396f, 0x00),
    (0x3970, 0x37),
    (0x3971, 0x37),
    (0x3972, 0x37),
    (0x3973, 0x37),
    (0x3974, 0x00),
    (0x3975, 0x3c),
    (0x3976, 0x3c),
    (0x3977, 0x3c),
    (0x3978, 0x3c),
    (0x3c00, 0x0f),
    (0x3c20, 0x01),
    (0x3c21, 0x08),
    (0x3f00, 0x8b),
    (0x3f02, 0x0f),
    (0x4000, 0xc3),
    (0x4001, 0xe0),
    (0x4002, 0x00),
    (0x4003, 0x40),
    (0x4008, 0x04),
    (0x4009, 0x23),
    (0x400a, 0x04),
    (0x400b, 0x01),
    (0x4041, 0x20),
    (0x4077, 0x06),
    (0x4078, 0x00),
    (0x4079, 0x1a),
    (0x407a, 0x7f),
    (0x407b, 0x01),
    (0x4080, 0x03),
    (0x4081, 0x84),
    (0x4308, 0x03),
    (0x4309, 0xff),
    (0x430d, 0x00),
    (0x4500, 0x07),
    (0x4501, 0x00),
    (0x4503, 0x00),
    (0x450a, 0x04),
    (0x450e, 0x00),
    (0x450f, 0x00),
    (0x4800, 0x64),
    (0x4806, 0x00),
    (0x4813, 0x00),
    (0x4815, 0x40),
    (0x4816, 0x12),
    (0x481f, 0x30),
    (0x4837, 0x14),
    (0x4857, 0x05),
    (0x4884, 0x04),
    (0x4900, 0x00),
    (0x4901, 0x00),
    (0x4902, 0x01),
    (0x4d00, 0x03),
    (0x4d01, 0xd8),
    (0x4d02, 0xba),
    (0x4d03, 0xa0),
    (0x4d04, 0xb7),
    (0x4d05, 0x34),
    (0x4d0d, 0x00),
    (0x5000, 0xfd),
    (0x5001, 0x50),
    (0x5006, 0x00),
    (0x5080, 0x40),
    (0x5181, 0x2b),
    (0x5202, 0xa3),
    (0x5206, 0x01),
    (0x5207, 0x00),
    (0x520a, 0x01),
    (0x520b, 0x00),
    (0x4f00, 0x01),
];
