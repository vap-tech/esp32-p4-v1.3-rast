//! Small fixed 5x7 clock renderer. No allocation; draws only changed glyphs.
use crate::devices::{framebuffer::Framebuffer, rx8025t::DateTime};
const BACKGROUND: u16 = 0x87e0;
const FOREGROUND: u16 = 0x0841;
const DIGITS: [[u8; 7]; 10] = [
    [14, 17, 19, 21, 25, 17, 14],
    [4, 12, 4, 4, 4, 4, 14],
    [14, 17, 1, 2, 4, 8, 31],
    [30, 1, 1, 14, 1, 1, 30],
    [2, 6, 10, 18, 31, 2, 2],
    [31, 16, 16, 30, 1, 1, 30],
    [14, 16, 16, 30, 17, 17, 14],
    [31, 1, 2, 4, 8, 8, 8],
    [14, 17, 17, 14, 17, 17, 14],
    [14, 17, 17, 15, 1, 1, 14],
];
fn glyph(c: u8) -> [u8; 7] {
    match c {
        b'0'..=b'9' => DIGITS[usize::from(c - b'0')],
        b':' => [0, 4, 4, 0, 4, 4, 0],
        b'-' => [0, 0, 0, 31, 0, 0, 0],
        b'M' => [17, 27, 21, 21, 17, 17, 17],
        b'S' => [15, 16, 16, 14, 1, 1, 30],
        b'T' => [31, 4, 4, 4, 4, 4, 4],
        b'O' => [14, 17, 17, 17, 17, 17, 14],
        b'P' => [30, 17, 17, 30, 16, 16, 16],
        b'K' => [17, 18, 20, 24, 20, 18, 17],
        _ => [0; 7],
    }
}
fn digit_pair(n: u8, out: &mut [u8]) {
    out[0] = b'0' + n / 10;
    out[1] = b'0' + n % 10;
}
/// UTC -> fixed UTC+3, including date and weekday rollover. No DST.
pub fn moscow(mut date: DateTime) -> DateTime {
    date.hour += 3;
    if date.hour < 24 {
        return date;
    }
    date.hour -= 24;
    date.weekday = (date.weekday + 1) % 7;
    let leap = date.year.is_multiple_of(4)
        && (!date.year.is_multiple_of(100) || date.year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
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
    date.day += 1;
    if date.day > days[usize::from(date.month - 1)] {
        date.day = 1;
        date.month += 1;
    }
    if date.month > 12 {
        date.month = 1;
        date.year += 1;
    }
    date
}
#[derive(Default)]
pub struct ClockFace {
    previous: Option<([u8; 8], [u8; 10])>,
}
impl ClockFace {
    pub fn invalidate(&mut self) {
        self.previous = None;
    }
    pub fn draw(&mut self, fb: &mut Framebuffer, utc: Option<DateTime>) -> usize {
        let mut time = *b"--:--:--";
        let mut date = *b"---- -- --";
        if let Some(t) = utc {
            let t = moscow(t);
            digit_pair(t.hour, &mut time[0..2]);
            digit_pair(t.minute, &mut time[3..5]);
            digit_pair(t.second, &mut time[6..8]);
            digit_pair((t.year / 100) as u8, &mut date[0..2]);
            digit_pair((t.year % 100) as u8, &mut date[2..4]);
            date[4] = b'-';
            date[7] = b'-';
            digit_pair(t.month, &mut date[5..7]);
            digit_pair(t.day, &mut date[8..10]);
        }
        let old_time = self.previous.map(|p| p.0);
        let old_date = self.previous.map(|p| p.1);
        let mut count = 0;
        for (i, &c) in time.iter().enumerate() {
            if old_time.is_none_or(|p| p[i] != c) {
                draw_char(fb, 128 + i * 96, 210, c, 16);
                count += 1;
            }
        }
        for (i, &c) in date.iter().enumerate() {
            if old_date.is_none_or(|p| p[i] != c) {
                draw_char(fb, 332 + i * 36, 350, c, 6);
                count += 1;
            }
        }
        if self.previous.is_none() {
            for (i, c) in b"MSK".iter().enumerate() {
                draw_char(fb, 458 + i * 36, 410, *c, 6);
            }
        }
        fb.finish_writes();
        self.previous = Some((time, date));
        count
    }
}
fn draw_char(fb: &mut Framebuffer, x: usize, y: usize, c: u8, scale: usize) {
    let bits = glyph(c);
    let mut row = [BACKGROUND; 96]; // 6 columns incl. spacing, max scale=16.
    for (gy, bits) in bits.into_iter().chain(core::iter::once(0)).enumerate() {
        row.fill(BACKGROUND);
        if gy < 7 {
            for gx in 0..5 {
                if bits & (1 << (4 - gx)) != 0 {
                    row[gx * scale..(gx + 1) * scale].fill(FOREGROUND);
                }
            }
        }
        for sy in 0..scale {
            fb.write_run(x, y + gy * scale + sy, &row[..6 * scale]);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timezone_rollovers() {
        for (input, expected) in [
            ("2024-02-28T23:59:59", (2024, 2, 29)),
            ("2023-02-28T21:00:00", (2023, 3, 1)),
            ("2026-12-31T22:00:00", (2027, 1, 1)),
            ("2099-12-31T23:00:00", (2100, 1, 1)),
        ] {
            let utc = DateTime::parse(input).unwrap();
            let t = moscow(utc);
            assert_eq!((t.year, t.month, t.day), expected);
            assert_eq!(t.hour, utc.hour - 21);
            assert_eq!(t.weekday, (utc.weekday + 1) % 7);
        }
        let t = DateTime::parse("2026-11-10T12:34:56").unwrap();
        assert_eq!(moscow(t).hour, 15);
    }
}

pub fn stop_hit(x: u16, y: u16) -> bool {
    (312..712).contains(&x) && (480..568).contains(&y)
}
pub fn alarm_button(fb: &mut Framebuffer, visible: bool) {
    let color = if visible { 0xf980 } else { BACKGROUND };
    let row = [color; 400];
    for y in 480..568 {
        fb.write_run(312, y, &row);
    }
    if visible {
        // Text cells retain the clock's background; colored border marks the target.
        for (i, c) in b"STOP".iter().enumerate() {
            draw_char(fb, 416 + i * 48, 492, *c, 8);
        }
    }
    fb.finish_writes();
}
#[cfg(test)]
mod alarm_tests {
    #[test]
    fn stop_button_bounds() {
        assert!(super::stop_hit(312, 480));
        assert!(super::stop_hit(711, 567));
        for p in [(311, 480), (712, 500), (400, 479), (400, 568), (0, 0)] {
            assert!(!super::stop_hit(p.0, p.1));
        }
    }
}
