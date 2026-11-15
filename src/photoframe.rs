//! Bounded web photoframe settings and SRM geometry, independent of board I/O.
use crate::hal::ppa::{Rect, Rotation, Srm, Transform};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub interval: u32,
    pub seconds: u32,
    pub fill: bool,
    /// Mirror, alternate 180 degrees, wipe, slide. No BLEND.
    pub effects: u8,
}
impl Options {
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(',');
        let interval = parts.next()?.parse().ok()?;
        let seconds = parts.next()?.parse().ok()?;
        let fill = match parts.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let effects = parts.next()?.parse().ok()?;
        if parts.next().is_some()
            || !(2..=3600).contains(&interval)
            || seconds > 86400
            || effects > 15
        {
            return None;
        }
        Some(Self {
            interval,
            seconds,
            fill,
            effects,
        })
    }
    pub fn layout(self, width: u16, height: u16, index: usize) -> Option<Srm> {
        if width == 0 || height == 0 || width & 1 != 0 {
            return None;
        }
        let w = u32::from(width);
        let h = u32::from(height);
        let q = if self.fill {
            (1024u32 * 16).div_ceil(w).max((600u32 * 16).div_ceil(h))
        } else {
            (1024 * 16 / w).min(600 * 16 / h)
        }
        .clamp(1, 4095);
        let mut cw = if self.fill { w.min(1024 * 16 / q) } else { w } & !1;
        let ch = if self.fill { h.min(600 * 16 / q) } else { h };
        while cw > 0 && (cw * q / 16) & 1 != 0 {
            cw -= 2;
        }
        let ow = cw * q / 16;
        let oh = ch * q / 16;
        if ow == 0 || oh == 0 || ow > 1024 || oh > 600 {
            return None;
        }
        let cfg = Srm {
            source_size: (width, height),
            source: Rect {
                x: ((w - cw) / 2) as u16 & !1,
                y: ((h - ch) / 2) as u16,
                width: cw as u16,
                height: ch as u16,
            },
            destination_size: (1024, 600),
            destination: (((1024 - ow) / 2) as u16 & !1, ((600 - oh) / 2) as u16),
            transform: Transform {
                scale_x: q as u16,
                scale_y: q as u16,
                rotation: if self.effects & 2 != 0 && index & 1 != 0 {
                    Rotation::Deg180
                } else {
                    Rotation::Deg0
                },
                mirror_x: self.effects & 1 != 0,
                mirror_y: false,
            },
        };
        cfg.output_rect().map(|_| cfg)
    }
}
pub fn is_jpeg(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unbounded_or_ambiguous_options() {
        for p in [
            "",
            "1,0,0,0",
            "3601,0,0,0",
            "10,86401,0,0",
            "10,0,2,0",
            "10,0,0,16",
            "10,0,0,0,0",
            "10,-1,0,0",
        ] {
            assert!(Options::parse(p).is_none(), "{p}");
        }
        assert_eq!(Options::parse("10,0,0,15").unwrap().seconds, 0);
    }
    #[test]
    fn layouts_stay_within_source_and_display() {
        for fill in [false, true] {
            let o = Options {
                interval: 10,
                seconds: 0,
                fill,
                effects: 3,
            };
            for (w, h) in [
                (1536, 1024),
                (1024, 1536),
                (1024, 600),
                (32, 16),
                (2048, 2048),
                (16, 2048),
                (2048, 16),
            ] {
                let c = o.layout(w, h, 1).unwrap();
                assert!(c.source.fits(w, h));
                assert!(c.output_rect().unwrap().fits(1024, 600));
                assert!(matches!(c.transform.rotation, Rotation::Deg180));
            }
        }
    }
    #[test]
    fn jpg_filter_is_not_substring_match() {
        assert!(is_jpeg("Photo.JpEg"));
        assert!(!is_jpeg("a.jpg.bak"));
        assert!(!is_jpeg("jpg"));
    }
}
