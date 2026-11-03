//! GT911 on the board's shared I2C1. Polling; existing configuration is read-only.
use crate::hal::{
    i2c::{self, I2c},
    time::Timer,
};

const ADDRESS: u8 = 0x5d;
const STATUS: u16 = 0x814e;
const POINTS: u16 = 0x814f;
const MAX_POINTS: usize = 5;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Bus(i2c::Error),
    WrongProduct,
    InvalidFrame,
}
impl From<i2c::Error> for Error {
    fn from(error: i2c::Error) -> Self {
        Self::Bus(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub firmware: u16,
    pub config_version: u8,
    pub width: u16,
    pub height: u16,
    pub max_contacts: u8,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Point {
    pub id: u8,
    pub x: u16,
    pub y: u16,
    pub size: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    points: [Point; MAX_POINTS],
    count: u8,
    pub timestamp_us: u64,
}
impl Frame {
    pub fn points(&self) -> &[Point] {
        &self.points[..usize::from(self.count)]
    }

    fn decode(data: &[u8], count: u8, timestamp_us: u64) -> Result<Self, Error> {
        if usize::from(count) > MAX_POINTS || data.len() != usize::from(count) * 8 {
            return Err(Error::InvalidFrame);
        }
        let mut frame = Self {
            points: [Point::default(); MAX_POINTS],
            count,
            timestamp_us,
        };
        for (index, bytes) in data.as_chunks::<8>().0.iter().enumerate() {
            let id = bytes[0] & 0x0f;
            if frame.points[..index].iter().any(|point| point.id == id) {
                return Err(Error::InvalidFrame);
            }
            frame.points[index] = Point {
                id,
                x: u16::from_le_bytes([bytes[1], bytes[2]]),
                y: u16::from_le_bytes([bytes[3], bytes[4]]),
                size: u16::from_le_bytes([bytes[5], bytes[6]]),
            };
        }
        Ok(frame)
    }
}

pub struct Gt911 {
    info: Info,
}
impl Gt911 {
    /// Identify the controller and read its existing coordinate configuration.
    /// Borrows the shared bus only for transactions; does not reset/reconfigure it.
    pub fn new(bus: &mut I2c, timer: &mut Timer) -> Result<Self, Error> {
        let mut id = [0u8; 6];
        Self::read(bus, timer, 0x8140, &mut id)?;
        if &id[..4] != b"911\0" {
            return Err(Error::WrongProduct);
        }
        let mut config = [0u8; 6];
        Self::read(bus, timer, 0x8047, &mut config)?;
        Ok(Self {
            info: Info {
                firmware: u16::from_le_bytes([id[4], id[5]]),
                config_version: config[0],
                width: u16::from_le_bytes([config[1], config[2]]),
                height: u16::from_le_bytes([config[3], config[4]]),
                max_contacts: config[5] & 0x0f,
            },
        })
    }

    pub fn info(&self) -> Info {
        self.info
    }

    fn read(bus: &mut I2c, timer: &mut Timer, reg: u16, data: &mut [u8]) -> Result<(), Error> {
        bus.write_read(ADDRESS, &reg.to_be_bytes(), data, timer)?;
        Ok(())
    }

    fn acknowledge(bus: &mut I2c, timer: &mut Timer) -> Result<(), Error> {
        // Clear only the volatile data-ready status; never write configuration.
        bus.write_read(ADDRESS, &[0x81, 0x4e, 0], &mut [], timer)?;
        Ok(())
    }

    /// None means no fresh frame, not a finger release. A fresh zero-contact
    /// frame is Some(Frame). Returns only after acknowledging the frame.
    /// On an I2C read error, leave status pending for retry; on malformed data,
    /// discard/acknowledge the frame and return InvalidFrame.
    pub fn read_frame(&mut self, bus: &mut I2c, timer: &mut Timer) -> Result<Option<Frame>, Error> {
        let mut status = [0u8; 1];
        Self::read(bus, timer, STATUS, &mut status)?;
        if status[0] & 0x80 == 0 {
            return Ok(None);
        }
        let count = status[0] & 0x0f;
        if usize::from(count) > MAX_POINTS {
            Self::acknowledge(bus, timer)?;
            return Err(Error::InvalidFrame);
        }
        let mut bytes = [0u8; MAX_POINTS * 8];
        let length = usize::from(count) * 8;
        // Five contacts need 40 bytes; the HAL accepts up to 32 per read.
        // GT911 holds a report until status is acknowledged after both reads.
        for offset in (0..length).step_by(32) {
            let end = (offset + 32).min(length);
            Self::read(bus, timer, POINTS + offset as u16, &mut bytes[offset..end])?;
        }
        let frame = Frame::decode(&bytes[..length], count, timer.now_us());
        Self::acknowledge(bus, timer)?;
        frame.map(Some)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Down,
    Move,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    pub point: Point,
    pub timestamp_us: u64,
}

/// Single-pointer events over up to five reported contacts. Keeps the selected
/// ID while present, irrespective of report ordering. On handover returns UP
/// before DOWN. No allocation, synthetic timeouts, or coordinate transformation.
#[derive(Default)]
pub struct Tracker {
    active: Option<Point>,
}
impl Tracker {
    pub fn update(&mut self, frame: Option<Frame>) -> [Option<Event>; 2] {
        let Some(frame) = frame else {
            return [None, None];
        };
        let previous = self.active;
        let next = previous
            .and_then(|old| frame.points().iter().find(|p| p.id == old.id).copied())
            .or_else(|| frame.points().iter().min_by_key(|p| p.id).copied());
        let event = |kind, point| {
            Some(Event {
                kind,
                point,
                timestamp_us: frame.timestamp_us,
            })
        };
        let events = match (previous, next) {
            (None, Some(point)) => [event(Kind::Down, point), None],
            (Some(point), None) => [event(Kind::Up, point), None],
            (Some(old), Some(point)) if old.id != point.id => {
                [event(Kind::Up, old), event(Kind::Down, point)]
            }
            (Some(old), Some(point)) if old.x != point.x || old.y != point.y => {
                [event(Kind::Move, point), None]
            }
            _ => [None, None],
        };
        self.active = next;
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn point(id: u8, x: u16, y: u16) -> Point {
        Point { id, x, y, size: 10 }
    }
    fn frame(points: &[Point]) -> Option<Frame> {
        let mut result = Frame {
            points: [Point::default(); 5],
            count: points.len() as u8,
            timestamp_us: 123,
        };
        result.points[..points.len()].copy_from_slice(points);
        Some(result)
    }
    #[test]
    fn tap_idle_move_release_and_deduplicate() {
        let mut tracker = Tracker::default();
        let a = point(2, 100, 200);
        let b = point(2, 101, 202);
        assert_eq!(tracker.update(frame(&[a]))[0].unwrap().kind, Kind::Down);
        assert_eq!(tracker.update(None), [None, None]);
        assert_eq!(tracker.update(frame(&[a])), [None, None]);
        let movement = tracker.update(frame(&[b]))[0].unwrap();
        assert_eq!(movement.kind, Kind::Move);
        assert_eq!(movement.point, b);
        let release = tracker.update(frame(&[]))[0].unwrap();
        assert_eq!(release.kind, Kind::Up);
        assert_eq!(release.point, b);
        assert_eq!(release.timestamp_us, 123);
        assert_eq!(tracker.update(frame(&[])), [None, None]);
    }
    #[test]
    fn stable_id_when_order_changes_then_handover() {
        let mut tracker = Tracker::default();
        let a = point(1, 10, 20);
        let b = point(2, 30, 40);
        assert_eq!(tracker.update(frame(&[b, a]))[0].unwrap().point, a);
        assert_eq!(tracker.update(frame(&[a, b])), [None, None]);
        let events = tracker.update(frame(&[b]));
        assert_eq!(events[0].unwrap().kind, Kind::Up);
        assert_eq!(events[0].unwrap().point, a);
        assert_eq!(events[1].unwrap().kind, Kind::Down);
        assert_eq!(events[1].unwrap().point, b);
    }
    #[test]
    fn decode_coordinates_size_and_ids() {
        let frame = Frame::decode(&[0x12, 0xff, 3, 0x57, 2, 0x34, 0x12, 0], 1, 42).unwrap();
        assert_eq!(
            frame.points()[0],
            Point {
                id: 2,
                x: 1023,
                y: 599,
                size: 0x1234
            }
        );
        assert_eq!(frame.timestamp_us, 42);
    }
    #[test]
    fn five_contacts_decode_with_reordered_ids() {
        let mut bytes = [0u8; 40];
        for index in 0..5 {
            bytes[index * 8] = (4 - index) as u8;
            bytes[index * 8 + 1..index * 8 + 3]
                .copy_from_slice(&(100 * index as u16).to_le_bytes());
            bytes[index * 8 + 3..index * 8 + 5].copy_from_slice(&(50 * index as u16).to_le_bytes());
        }
        let report = Frame::decode(&bytes, 5, 55).unwrap();
        assert_eq!(report.points().len(), 5);
        assert_eq!(report.points()[4].x, 400);
        assert_eq!(report.points()[4].y, 200);
        let mut tracker = Tracker::default();
        let event = tracker.update(Some(report))[0].unwrap();
        assert_eq!(event.point.id, 0);
        assert_eq!(event.point.x, 400);
    }

    #[test]
    fn reject_bad_lengths_counts_and_duplicate_ids() {
        assert!(Frame::decode(&[], 6, 0).is_err());
        assert!(Frame::decode(&[0; 7], 1, 0).is_err());
        assert!(Frame::decode(&[0; 16], 2, 0).is_err());
        assert!(Frame::decode(&[], 0, 0).unwrap().points().is_empty());
    }
}
