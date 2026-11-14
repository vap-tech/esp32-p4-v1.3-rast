//! Loop a validated PCM16 sample source into packed stereo I2S frames.
#[derive(Default, Debug)]
pub struct Cursor {
    at: usize,
    pub loops: u32,
}
impl Cursor {
    pub fn rewind(&mut self) {
        self.at = 0;
        self.loops = 0;
    }
    pub fn fill(
        &mut self,
        pcm: &[u8],
        channels: u16,
        frames: &mut [u32],
    ) -> Result<(), &'static str> {
        let stride = usize::from(channels) * 2;
        if !(1..=2).contains(&channels)
            || pcm.is_empty()
            || !pcm.len().is_multiple_of(stride)
            || self.at >= pcm.len()
            || !self.at.is_multiple_of(stride)
        {
            return Err("PCM alignment");
        }
        for out in frames {
            let left = u32::from(u16::from_le_bytes([pcm[self.at], pcm[self.at + 1]]));
            let right = if channels == 1 {
                left
            } else {
                u32::from(u16::from_le_bytes([pcm[self.at + 2], pcm[self.at + 3]]))
            };
            *out = left | right << 16;
            self.at += stride;
            if self.at == pcm.len() {
                self.at = 0;
                self.loops += 1;
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mono_wrap_and_resume_have_no_zero_gap() {
        let mut c = Cursor::default();
        let mut out = [0; 5];
        c.fill(&[1, 0, 0xff, 0xff], 1, &mut out).unwrap();
        assert_eq!(
            out,
            [0x00010001, 0xffffffff, 0x00010001, 0xffffffff, 0x00010001]
        );
        assert_eq!(c.loops, 2);
        c.fill(&[1, 0, 0xff, 0xff], 1, &mut out[..1]).unwrap();
        assert_eq!(out[0], 0xffffffff);
        c.rewind();
        c.fill(&[1, 0, 0xff, 0xff], 1, &mut out[..1]).unwrap();
        assert_eq!(out[0], 0x00010001);
    }
    #[test]
    fn stereo_order_and_invalid_sources() {
        let mut c = Cursor::default();
        let mut out = [0; 3];
        c.fill(&[0x34, 0x12, 0xdc, 0xfe], 2, &mut out).unwrap();
        assert_eq!(out, [0xfedc1234; 3]);
        for (bytes, ch) in [
            (&[][..], 1),
            (&[1][..], 1),
            (&[1, 2][..], 2),
            (&[1, 2][..], 0),
        ] {
            assert!(Cursor::default().fill(bytes, ch, &mut out).is_err());
        }
    }
}
