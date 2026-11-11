//! RX8025T-compatible RTC at 7-bit I2C address 0x32.
//! T uses direct register numbers; SA/NB's shifted addressing is incompatible.
use crate::hal::{
    i2c::{self, I2c},
    time::Timer,
};

#[derive(Debug)]
pub enum Error {
    Bus(i2c::Error),
    Unstable,
    InvalidCalendar,
    InvalidSetting,
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

/// Weekday convention: Sunday=0 .. Saturday=6. RTC has no timezone/DST.
impl DateTime {
    pub fn new(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self, InvalidCalendar> {
        if !(2000..=2099).contains(&year) {
            return Err(InvalidCalendar);
        }
        let mut raw = [0; 16];
        raw[..7].copy_from_slice(&[
            encode(second),
            encode(minute),
            encode(hour),
            1,
            encode(day),
            encode(month),
            encode((year - 2000) as u8),
        ]);
        // Reject ranges before encoding can hide an invalid decimal value.
        if month > 12 || day > 31 || hour > 23 || minute > 59 || second > 59 {
            return Err(InvalidCalendar);
        }
        let mut value = Snapshot { raw }.datetime()?;
        let offsets = [0u32, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let y = u32::from(year) - u32::from(month < 3);
        value.weekday = ((y + y / 4 - y / 100
            + y / 400
            + offsets[usize::from(month - 1)]
            + u32::from(day))
            % 7) as u8;
        Ok(value)
    }
    pub fn parse(text: &str) -> Result<Self, InvalidCalendar> {
        let b = text.as_bytes();
        if b.len() != 19
            || b[4] != b'-'
            || b[7] != b'-'
            || !matches!(b[10], b'T' | b' ')
            || b[13] != b':'
            || b[16] != b':'
        {
            return Err(InvalidCalendar);
        }
        let number = |a: usize, z: usize| -> Result<u16, InvalidCalendar> {
            let mut n = 0;
            for c in &b[a..z] {
                if !c.is_ascii_digit() {
                    return Err(InvalidCalendar);
                }
                n = n * 10 + u16::from(c - b'0');
            }
            Ok(n)
        };
        Self::new(
            number(0, 4)?,
            number(5, 7)? as u8,
            number(8, 10)? as u8,
            number(11, 13)? as u8,
            number(14, 16)? as u8,
            number(17, 19)? as u8,
        )
    }
    fn registers(self) -> Result<[u8; 7], Error> {
        let valid = Self::new(
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
        )
        .map_err(|_| Error::InvalidCalendar)?;
        if valid.weekday != self.weekday {
            return Err(Error::InvalidCalendar);
        }
        Ok([
            encode(self.second),
            encode(self.minute),
            encode(self.hour),
            1 << self.weekday,
            encode(self.day),
            encode(self.month),
            encode((self.year - 2000) as u8),
        ])
    }
}
fn encode(n: u8) -> u8 {
    ((n / 10) << 4) | (n % 10)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Compensation {
    HalfSecond,
    TwoSeconds,
    TenSeconds,
    ThirtySeconds,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Frequency {
    Hz32768,
    Hz1024,
    Hz1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TimerClock {
    Hz4096,
    Hz64,
    Second,
    Minute,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub compensation: Compensation,
    pub frequency: Frequency,
    pub update_each_minute: bool,
    pub update_interrupt: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            compensation: Compensation::TwoSeconds,
            frequency: Frequency::Hz32768,
            update_each_minute: false,
            update_interrupt: false,
        }
    }
}
impl Snapshot {
    pub fn settings(&self) -> Settings {
        Settings {
            compensation: match self.raw[15] >> 6 {
                0 => Compensation::HalfSecond,
                1 => Compensation::TwoSeconds,
                2 => Compensation::TenSeconds,
                _ => Compensation::ThirtySeconds,
            },
            frequency: match (self.raw[13] >> 2) & 3 {
                1 => Frequency::Hz1024,
                2 => Frequency::Hz1,
                _ => Frequency::Hz32768,
            },
            update_each_minute: self.raw[13] & 0x20 != 0,
            update_interrupt: self.raw[15] & 0x20 != 0,
        }
    }
    pub fn alarm_enabled(&self) -> bool {
        self.raw[15] & 8 != 0
    }
    pub fn alarm_pending(&self) -> bool {
        self.raw[14] & 8 != 0
    }
    pub fn timer_pending(&self) -> bool {
        self.raw[14] & 16 != 0
    }
    pub fn update_pending(&self) -> bool {
        self.raw[14] & 32 != 0
    }
    pub fn timer_count(&self) -> u16 {
        u16::from(self.raw[11]) | (u16::from(self.raw[12] & 15) << 8)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmDay {
    Any,
    Weekdays(u8),
    MonthDay(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alarm {
    pub minute: Option<u8>,
    pub hour: Option<u8>,
    pub day: AlarmDay,
    pub interrupt: bool,
}

pub struct Rx8025t;
impl Rx8025t {
    fn write(bus: &mut I2c, timer: &mut Timer, reg: u8, bytes: &[u8]) -> Result<(), Error> {
        let mut packet = [0; 17];
        packet[0] = reg;
        packet[1..1 + bytes.len()].copy_from_slice(bytes);
        bus.write_read(0x32, &packet[..1 + bytes.len()], &mut [], timer)?;
        Ok(())
    }
    /// Explicit first-use/recovery initialization; discards alarms, timer, RAM and flags.
    /// On a bus failure calendar/configuration may be partial: reinitialize before trusting it.
    pub fn initialize(
        bus: &mut I2c,
        timer: &mut Timer,
        date: DateTime,
        settings: Settings,
    ) -> Result<(), Error> {
        let calendar = date.registers()?;
        Self::write(bus, timer, 15, &[0x41])?;
        let mut registers = [0; 16];
        registers[..7].copy_from_slice(&calendar);
        registers[8..11].fill(0x80); // No alarm comparisons.
        registers[13] =
            ((settings.frequency as u8) << 2) | (u8::from(settings.update_each_minute) << 5);
        registers[15] =
            ((settings.compensation as u8) << 6) | (u8::from(settings.update_interrupt) << 5) | 1;
        Self::write(bus, timer, 0, &registers)?;
        Self::write(bus, timer, 15, &[registers[15] & !1])
    }
    /// Set calendar while retaining settings/alarm/timer. Does not clear VLF/VDET.
    pub fn set_datetime(bus: &mut I2c, timer: &mut Timer, date: DateTime) -> Result<(), Error> {
        let calendar = date.registers()?;
        let control = Self::read(bus, timer)?.raw[15] & 0xf8;
        Self::write(bus, timer, 15, &[control | 1])?;
        Self::write(bus, timer, 0, &calendar)?;
        Self::write(bus, timer, 15, &[control])
    }
    /// Preserve alarm/timer and RESET; force TEST and reserved control bits to zero.
    pub fn configure(bus: &mut I2c, timer: &mut Timer, settings: Settings) -> Result<(), Error> {
        let s = Self::read(bus, timer)?;
        let ext = (s.raw[13] & 0x53)
            | ((settings.frequency as u8) << 2)
            | (u8::from(settings.update_each_minute) << 5);
        let ctrl = (s.raw[15] & 0x19)
            | ((settings.compensation as u8) << 6)
            | (u8::from(settings.update_interrupt) << 5);
        Self::write(bus, timer, 13, &[ext])?;
        Self::write(bus, timer, 15, &[ctrl])
    }
    /// Acknowledge all latched events and voltage warnings; caller must first inspect them.
    pub fn clear_flags(bus: &mut I2c, timer: &mut Timer) -> Result<(), Error> {
        Self::write(bus, timer, 14, &[0])
    }
    /// Clear AF only (write-zero-to-clear); ones leave UF/TF/VLF/VDET untouched.
    /// Alarm comparisons and AIE remain armed for the next event.
    pub fn acknowledge_alarm(bus: &mut I2c, timer: &mut Timer) -> Result<(), Error> {
        Self::write(bus, timer, 14, &[0x33])
    }
    pub fn disable_alarm(bus: &mut I2c, timer: &mut Timer) -> Result<(), Error> {
        let control = Self::read(bus, timer)?.raw[15] & 0xf1;
        Self::write(bus, timer, 15, &[control])
    }
    pub fn write_ram(bus: &mut I2c, timer: &mut Timer, value: u8) -> Result<(), Error> {
        Self::write(bus, timer, 7, &[value])
    }
    pub fn set_alarm(bus: &mut I2c, timer: &mut Timer, alarm: Alarm) -> Result<(), Error> {
        if alarm.minute.is_some_and(|v| v > 59) || alarm.hour.is_some_and(|v| v > 23) {
            return Err(Error::InvalidSetting);
        }
        let (day, mode) = match alarm.day {
            AlarmDay::Any => (0x80, 0),
            AlarmDay::Weekdays(mask) if mask != 0 && mask & 0x80 == 0 => (mask, 0),
            AlarmDay::MonthDay(d) if (1..=31).contains(&d) => (encode(d), 0x40),
            _ => return Err(Error::InvalidSetting),
        };
        let s = Self::read(bus, timer)?;
        let control = s.raw[15] & 0xf1;
        Self::write(bus, timer, 15, &[control])?;
        Self::write(bus, timer, 13, &[(s.raw[13] & 0x3f) | mode])?;
        Self::write(
            bus,
            timer,
            8,
            &[
                alarm.minute.map_or(0x80, encode),
                alarm.hour.map_or(0x80, encode),
                day,
            ],
        )?;
        Self::acknowledge_alarm(bus, timer)?;
        // Publish the new comparisons only after clearing the prior alarm event.
        Self::write(
            bus,
            timer,
            15,
            &[control | (u8::from(alarm.interrupt) << 3)],
        )
    }
    /// count=None disables timer; otherwise 1..4095. Flags remain latched.
    pub fn set_timer(
        bus: &mut I2c,
        timer: &mut Timer,
        count: Option<u16>,
        clock: TimerClock,
        interrupt: bool,
    ) -> Result<(), Error> {
        if count.is_some_and(|v| !(1..=4095).contains(&v)) {
            return Err(Error::InvalidSetting);
        }
        let s = Self::read(bus, timer)?;
        let ext = (s.raw[13] & 0x6c) | clock as u8;
        let control = s.raw[15] & 0xe9;
        Self::write(bus, timer, 15, &[control])?;
        Self::write(bus, timer, 13, &[ext])?;
        if let Some(n) = count {
            Self::write(bus, timer, 11, &[n as u8, (n >> 8) as u8])?;
        }
        Self::write(
            bus,
            timer,
            15,
            &[control | (u8::from(interrupt && count.is_some()) << 4)],
        )?;
        Self::write(bus, timer, 13, &[ext | (u8::from(count.is_some()) << 4)])
    }
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
    #[test]
    fn century_calendar_roundtrip_and_weekdays() {
        let mut weekday = 6; // 2000-01-01 was Saturday.
        let mut count = 0;
        for year in 2000..=2099 {
            for month in 1..=12 {
                for day in 1..=31 {
                    if let Ok(date) = DateTime::new(year, month, day, 23, 59, 58) {
                        assert_eq!(date.weekday, weekday);
                        let mut raw = [0; 16];
                        raw[..7].copy_from_slice(&date.registers().unwrap());
                        assert_eq!(Snapshot { raw }.datetime().unwrap(), date);
                        weekday = (weekday + 1) % 7;
                        count += 1;
                    }
                }
            }
        }
        assert_eq!(count, 36525);
    }
    #[test]
    fn parse_and_reject_invalid_writes() {
        assert_eq!(DateTime::parse("2026-11-10T12:34:56").unwrap().weekday, 2);
        for text in [
            "2026-02-29T00:00:00",
            "2100-01-01T00:00:00",
            "2026-11-10T24:00:00",
            "2026-11-10T00:60:00",
            "2026-00-01T00:00:00",
            "2026-11-10T00:00:60",
            "2026/11/10T00:00:00",
            "2026-11-10T00:00:0é",
        ] {
            assert!(DateTime::parse(text).is_err(), "{}", text);
        }
        let mut wrong = DateTime::parse("2026-11-10T12:34:56").unwrap();
        wrong.weekday = 7;
        assert!(wrong.registers().is_err());
    }
    #[test]
    fn decode_settings_and_events() {
        let mut s = valid();
        s.raw[13] = 0x28;
        s.raw[14] = 0x38;
        s.raw[15] = 0xa0;
        assert_eq!(
            s.settings(),
            Settings {
                compensation: Compensation::TenSeconds,
                frequency: Frequency::Hz1,
                update_each_minute: true,
                update_interrupt: true
            }
        );
        assert!(s.alarm_pending() && s.timer_pending() && s.update_pending());
    }
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
