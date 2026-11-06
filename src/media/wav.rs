//! RIFF/WAVE PCM16 parser: bounds-checked chunks, unknown chunks and odd padding.
use super::{read_exact, Error, Input};
#[derive(Debug, Clone, Copy)]
pub struct Wave {
    pub sample_rate: u32,
    pub channels: u16,
    pub data_offset: u32,
    pub data_bytes: u32,
}
impl Wave {
    pub fn parse<R: Input>(input: &mut R) -> Result<Self, Error<R::Error>> {
        input.seek(0).map_err(Error::Io)?;
        let mut header = [0; 12];
        read_exact(input, &mut header)?;
        if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
            return Err(Error::Invalid("RIFF/WAVE signature"));
        }
        let end = u32::from_le_bytes(header[4..8].try_into().unwrap())
            .checked_add(8)
            .filter(|&end| end <= input.len() && end >= 12)
            .ok_or(Error::Invalid("RIFF size"))?;
        let mut format = None;
        let mut data = None;
        while input.position() <= end.saturating_sub(8) {
            let mut chunk = [0; 8];
            read_exact(input, &mut chunk)?;
            let size = u32::from_le_bytes(chunk[4..].try_into().unwrap());
            let start = input.position();
            let next = start
                .checked_add(size)
                .and_then(|n| n.checked_add(size & 1))
                .filter(|&n| n <= end)
                .ok_or(Error::Invalid("WAV chunk bounds"))?;
            match &chunk[..4] {
                b"fmt " => {
                    if format.is_some() || size < 16 {
                        return Err(Error::Invalid("WAV fmt"));
                    }
                    let mut fmt = [0; 16];
                    read_exact(input, &mut fmt)?;
                    let value = |i| u16::from_le_bytes([fmt[i], fmt[i + 1]]);
                    let rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
                    let bytes = u32::from_le_bytes(fmt[8..12].try_into().unwrap());
                    let channels = value(2);
                    if value(0) != 1 || value(14) != 16 || !(1..=2).contains(&channels) {
                        return Err(Error::Unsupported("WAV requires PCM16 mono/stereo"));
                    }
                    if rate == 0
                        || rate > 192_000
                        || value(12) != channels * 2
                        || bytes != rate * u32::from(channels) * 2
                    {
                        return Err(Error::Invalid("WAV rate/block alignment"));
                    }
                    format = Some((rate, channels));
                }
                b"data" => {
                    if data.is_some() {
                        return Err(Error::Unsupported("multiple WAV data chunks"));
                    }
                    data = Some((start, size));
                }
                _ => {}
            }
            input.seek(next).map_err(Error::Io)?;
        }
        let (sample_rate, channels) = format.ok_or(Error::Invalid("missing WAV fmt"))?;
        let (data_offset, data_bytes) = data.ok_or(Error::Invalid("missing WAV data"))?;
        if data_bytes == 0 || !data_bytes.is_multiple_of(u32::from(channels) * 2) {
            return Err(Error::Invalid("WAV data alignment"));
        }
        input.seek(data_offset).map_err(Error::Io)?;
        Ok(Self {
            sample_rate,
            channels,
            data_offset,
            data_bytes,
        })
    }
    /// Read up to 512 stereo frames; mono is duplicated, tail is zero-padded.
    pub fn read_frames<R: Input>(
        &self,
        input: &mut R,
        frames: &mut [u32; 512],
    ) -> Result<usize, Error<R::Error>> {
        self.read_frames_with_scratch(input, frames, &mut [0; 2048])
    }
    /// Same PCM expansion with caller-owned scratch for deeply nested render loops.
    pub fn read_frames_with_scratch<R: Input>(
        &self,
        input: &mut R,
        frames: &mut [u32; 512],
        raw: &mut [u8; 2048],
    ) -> Result<usize, Error<R::Error>> {
        let position = input.position();
        let end = self.data_offset + self.data_bytes;
        if position < self.data_offset || position > end {
            return Err(Error::Invalid("WAV cursor"));
        }
        let stride = usize::from(self.channels) * 2;
        let n = ((end - position) as usize / stride).min(frames.len());
        read_exact(input, &mut raw[..n * stride])?;
        for (i, frame) in frames.iter_mut().enumerate() {
            *frame = if i < n {
                let left = u32::from(u16::from_le_bytes([raw[i * stride], raw[i * stride + 1]]));
                let right = if stride == 2 {
                    left
                } else {
                    u32::from(u16::from_le_bytes([
                        raw[i * stride + 2],
                        raw[i * stride + 3],
                    ]))
                };
                left | (right << 16)
            } else {
                0
            };
        }
        Ok(n)
    }
}
#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::media::tests::Bytes;
    fn fixture() -> std::vec::Vec<u8> {
        let mut b = b"RIFF\x00\x00\x00\x00WAVEJUNK\x01\x00\x00\x00x\x00fmt \x10\x00\x00\x00\x01\x00\x01\x00\x80\x3e\x00\x00\x00\x7d\x00\x00\x02\x00\x10\x00data\x04\x00\x00\x00\x34\x12\x00\x80".to_vec();
        let n = (b.len() - 8) as u32;
        b[4..8].copy_from_slice(&n.to_le_bytes());
        b
    }
    #[test]
    fn pcm_chunks_padding_and_mono_expansion() {
        let data = fixture();
        let mut input = Bytes {
            data: &data,
            pos: 0,
        };
        let wav = Wave::parse(&mut input).unwrap();
        assert_eq!(
            (wav.sample_rate, wav.channels, wav.data_bytes),
            (16000, 1, 4)
        );
        let mut frames = [1; 512];
        assert_eq!(wav.read_frames(&mut input, &mut frames).unwrap(), 2);
        assert_eq!(&frames[..3], &[0x12341234, 0x80008000, 0]);
        assert_eq!(wav.read_frames(&mut input, &mut frames).unwrap(), 0);
    }
    #[test]
    fn rejects_truncation_and_chunk_overflow() {
        let mut data = fixture();
        data.pop();
        assert!(Wave::parse(&mut Bytes {
            data: &data,
            pos: 0
        })
        .is_err());
        let mut data = fixture();
        data[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Wave::parse(&mut Bytes {
            data: &data,
            pos: 0
        })
        .is_err());
    }
}
