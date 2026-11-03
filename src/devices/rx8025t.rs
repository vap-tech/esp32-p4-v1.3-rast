//! Read-only RX8025T-compatible RTC at 7-bit I2C address 0x32.
//! T uses direct register numbers; SA/NB's shifted addressing is incompatible.
use crate::hal::{
    i2c::{self, I2c},
    time::Timer,
};

#[derive(Debug)]
pub enum Error {
    Bus(i2c::Error),
    Unstable,
}
impl From<i2c::Error> for Error {
    fn from(error: i2c::Error) -> Self {
        Self::Bus(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    /// Driver policy: interpret the two-digit year as 2000..2099.
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidCalendar;

pub struct Snapshot {
    pub raw: [u8; 16],
}
impl Snapshot {
    pub fn voltage_low(&self) -> bool {
        self.raw[0x0e] & 2 != 0
    }
    pub fn voltage_detected(&self) -> bool {
        self.raw[0x0e] & 1 != 0
    }
    pub fn stopped(&self) -> bool {
        self.raw[0x0f] & 1 != 0
    }
    pub fn test_mode(&self) -> bool {
        self.raw[0x0d] & 0x80 != 0
    }
    pub fn time_usable(&self) -> bool {
        self.datetime().is_ok() && !self.voltage_low() && !self.stopped() && !self.test_mode()
    }

    pub fn datetime(&self) -> Result<DateTime, InvalidCalendar> {
        let r = &self.raw;
        if r[0] & 0x80 != 0
            || r[1] & 0x80 != 0
            || r[2] & 0xc0 != 0
            || r[3] & 0x80 != 0
            || r[4] & 0xc0 != 0
            || r[5] & 0xe0 != 0
            || r[3].count_ones() != 1
        {
            return Err(InvalidCalendar);
        }
        let second = bcd(r[0])?;
        let minute = bcd(r[1])?;
        let hour = bcd(r[2])?;
        let day = bcd(r[4])?;
        let month = bcd(r[5])?;
        let year = 2000 + u16::from(bcd(r[6])?);
        if second > 59 || minute > 59 || hour > 23 || !(1..=12).contains(&month) {
            return Err(InvalidCalendar);
        }
        let days = [
            31,
            if year % 4 == 0 { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        if day == 0 || day > days[usize::from(month - 1)] {
            return Err(InvalidCalendar);
        }
        Ok(DateTime {
            year,
            month,
            day,
            weekday: r[3].trailing_zeros() as u8,
            hour,
            minute,
            second,
        })
    }
}

fn bcd(value: u8) -> Result<u8, InvalidCalendar> {
    if value & 15 > 9 || value >> 4 > 9 {
        return Err(InvalidCalendar);
    }
    Ok((value >> 4) * 10 + (value & 15))
}

pub struct Rx8025t;
impl Rx8025t {
    /// Read registers only. No initialization, flag clearing, or time setting.
    /// Recheck seconds after the block to avoid a mixed calendar at rollover.
    pub fn read(bus: &mut I2c, timer: &mut Timer) -> Result<Snapshot, Error> {
        for _ in 0..3 {
            let mut raw = [0; 16];
            bus.write_read(0x32, &[0], &mut raw, timer)?;
            let mut second = [0];
            bus.write_read(0x32, &[0], &mut second, timer)?;
            if raw[0] == second[0] {
                return Ok(Snapshot { raw });
            }
        }
        Err(Error::Unstable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Snapshot {
        let mut raw = [0; 16];
        raw[..7].copy_from_slice(&[0x59, 0x58, 0x23, 0x08, 0x29, 0x02, 0x24]);
        Snapshot { raw }
    }
    #[test]
    fn leap_day_and_bcd() {
        let s = valid();
        let date = s.datetime().unwrap();
        assert_eq!(
            (
                date.year,
                date.month,
                date.day,
                date.hour,
                date.minute,
                date.second
            ),
            (2024, 2, 29, 23, 58, 59)
        );
        assert!(s.time_usable());
    }
    #[test]
    fn reject_bad_calendar() {
        for (index, value) in [
            (0, 0x6a),
            (2, 0x24),
            (3, 0x7f),
            (4, 0),
            (5, 0x13),
            (6, 0xf4),
        ] {
            let mut s = valid();
            s.raw[index] = value;
            assert!(s.datetime().is_err());
        }
        let mut s = valid();
        s.raw[6] = 0x23;
        assert!(s.datetime().is_err()); // 2023-02-29.
    }
    #[test]
    fn flags_never_make_bad_calendar_valid() {
        for (reg, mask) in [(14, 2), (15, 1), (13, 128)] {
            let mut s = valid();
            s.raw[reg] |= mask;
            assert!(!s.time_usable());
        }
        let mut s = valid();
        s.raw[14] = 1;
        assert!(s.voltage_detected());
        assert!(s.time_usable()); // VDET warns about compensation, not data loss.
        s.raw[6] = 0xf4;
        s.raw[14] = 0;
        assert!(!s.time_usable());
    }
}
