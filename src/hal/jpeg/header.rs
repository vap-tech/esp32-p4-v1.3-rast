//! Strict baseline JPEG metadata for the P4 decoder. DMA starts at SOS marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Invalid(&'static str),
    Unsupported(&'static str),
}
const ZIG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];
#[derive(Clone, Copy)]
pub struct Huffman {
    pub counts: [u8; 16],
    pub symbols: [u8; 256],
    pub present: bool,
}
impl Huffman {
    const EMPTY: Self = Self {
        counts: [0; 16],
        symbols: [0; 256],
        present: false,
    };
}
#[derive(Clone, Copy, Default)]
pub struct Component {
    pub id: u8,
    pub h: u8,
    pub v: u8,
    pub quant: u8,
}
pub struct Header {
    pub width: u16,
    pub height: u16,
    pub stride: u16,
    pub rows: u16,
    pub components: u8,
    pub component: [Component; 3],
    pub quant: [[u16; 64]; 4],
    pub qmask: u8,
    pub huffman: [[Huffman; 2]; 2],
    pub restart: u16,
    pub sos: usize,
}
impl Header {
    pub fn output_bytes(&self) -> usize {
        usize::from(self.stride) * usize::from(self.rows) * if self.components == 1 { 1 } else { 2 }
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < 4 {
            return Err(Error::Truncated);
        }
        if bytes[..2] != [255, 216] || bytes[bytes.len() - 2..] != [255, 217] {
            return Err(Error::Invalid("SOI/EOI"));
        }
        let mut s = Self {
            width: 0,
            height: 0,
            stride: 0,
            rows: 0,
            components: 0,
            component: [Component::default(); 3],
            quant: [[0; 64]; 4],
            qmask: 0,
            huffman: [[Huffman::EMPTY; 2]; 2],
            restart: 0,
            sos: 0,
        };
        let mut at = 2;
        loop {
            let start = at;
            if *bytes.get(at).ok_or(Error::Truncated)? != 255 {
                return Err(Error::Invalid("marker"));
            }
            at += 1;
            while bytes.get(at) == Some(&255) {
                at += 1;
            }
            let marker = *bytes.get(at).ok_or(Error::Truncated)?;
            at += 1;
            let length = be(bytes, at)? as usize;
            if length < 2 {
                return Err(Error::Invalid("segment length"));
            }
            let end = at
                .checked_add(length)
                .filter(|n| *n <= bytes.len())
                .ok_or(Error::Truncated)?;
            let p = &bytes[at + 2..end];
            at = end;
            match marker {
                0xe0..=0xef | 0xfe => {}
                0xdb => {
                    let mut n = 0;
                    while n < p.len() {
                        let spec = p[n];
                        n += 1;
                        let id = (spec & 15) as usize;
                        if spec >> 4 != 0 || id >= 4 {
                            return Err(Error::Unsupported("quantization precision/table"));
                        }
                        let q = p.get(n..n + 64).ok_or(Error::Truncated)?;
                        for (i, v) in q.iter().enumerate() {
                            if *v == 0 {
                                return Err(Error::Invalid("zero quantizer"));
                            }
                            s.quant[id][ZIG[i]] = u16::from(*v);
                        }
                        s.qmask |= 1 << id;
                        n += 64;
                    }
                }
                0xc0 => {
                    if s.components != 0 || p.len() < 6 {
                        return Err(Error::Invalid("SOF"));
                    }
                    if p[0] != 8 {
                        return Err(Error::Unsupported("sample precision"));
                    }
                    s.height = be(p, 1)?;
                    s.width = be(p, 3)?;
                    s.components = p[5];
                    if ![1, 3].contains(&s.components)
                        || p.len() != 6 + 3 * usize::from(s.components)
                    {
                        return Err(Error::Unsupported("component count"));
                    }
                    if s.width == 0
                        || s.height == 0
                        || !s.width.is_multiple_of(8)
                        || !s.height.is_multiple_of(8)
                    {
                        return Err(Error::Unsupported(
                            "rev1.x dimensions must be multiples of 8",
                        ));
                    }
                    for i in 0..usize::from(s.components) {
                        let c = &p[6 + i * 3..9 + i * 3];
                        let h = c[1] >> 4;
                        let v = c[1] & 15;
                        if c[2] >= 4
                            || (i == 0 && ![0x11, 0x21, 0x22].contains(&c[1]))
                            || (i != 0 && c[1] != 0x11)
                            || s.component[..i].iter().any(|a| a.id == c[0])
                        {
                            return Err(Error::Unsupported("sampling/component"));
                        }
                        if s.components == 1 && c[1] != 0x11 {
                            return Err(Error::Unsupported("gray sampling"));
                        }
                        s.component[i] = Component {
                            id: c[0],
                            h,
                            v,
                            quant: c[2],
                        };
                    }
                    let mx = u16::from(s.component[0].h) * 8;
                    let my = u16::from(s.component[0].v) * 8;
                    s.stride = s
                        .width
                        .checked_add(mx - 1)
                        .ok_or(Error::Invalid("width overflow"))?
                        / mx
                        * mx;
                    s.rows = s
                        .height
                        .checked_add(my - 1)
                        .ok_or(Error::Invalid("height overflow"))?
                        / my
                        * my;
                    if s.stride > 16383 || s.rows > 16383 {
                        return Err(Error::Unsupported("DMA dimensions"));
                    }
                }
                0xc4 => {
                    let mut n = 0;
                    while n < p.len() {
                        let spec = p[n];
                        n += 1;
                        let class = (spec >> 4) as usize;
                        let id = (spec & 15) as usize;
                        if class > 1 || id > 1 {
                            return Err(Error::Unsupported("Huffman table"));
                        }
                        let counts = p.get(n..n + 16).ok_or(Error::Truncated)?;
                        n += 16;
                        let mut total = 0;
                        let mut space = 1i32;
                        for &c in counts {
                            space = space * 2 - i32::from(c);
                            if space < 0 {
                                return Err(Error::Invalid("oversubscribed Huffman"));
                            }
                            total += usize::from(c);
                        }
                        if total == 0 || total > if class == 0 { 12 } else { 256 } {
                            return Err(Error::Invalid("Huffman symbol count"));
                        }
                        let values = p.get(n..n + total).ok_or(Error::Truncated)?;
                        n += total;
                        if values.iter().any(|&v| {
                            if class == 0 {
                                v > 11
                            } else {
                                v & 15 > 10 || (v & 15 == 0 && v != 0 && v != 0xf0)
                            }
                        }) {
                            return Err(Error::Invalid("Huffman symbol"));
                        }
                        let t = &mut s.huffman[class][id];
                        *t = Huffman::EMPTY;
                        t.counts.copy_from_slice(counts);
                        t.symbols[..total].copy_from_slice(values);
                        t.present = true;
                    }
                }
                0xdd => {
                    if p.len() != 2 {
                        return Err(Error::Invalid("DRI"));
                    }
                    s.restart = be(p, 0)?;
                }
                0xda => {
                    if s.components == 0
                        || p.len() != 4 + 2 * usize::from(s.components)
                        || p[0] != s.components
                    {
                        return Err(Error::Unsupported("single interleaved scan required"));
                    }
                    for i in 0..usize::from(s.components) {
                        let c = s.component[i];
                        let tables = p[2 + i * 2];
                        if p[1 + i * 2] != c.id
                            || tables >> 4 > 1
                            || tables & 15 > 1
                            || !s.huffman[0][(tables >> 4) as usize].present
                            || !s.huffman[1][(tables & 15) as usize].present
                            || s.qmask & (1 << c.quant) == 0
                        {
                            return Err(Error::Invalid("scan component/table"));
                        }
                    }
                    if p[p.len() - 3..] != [0, 63, 0] {
                        return Err(Error::Unsupported("non-baseline scan"));
                    }
                    s.sos = start;
                    return Ok(s);
                }
                0xc1..=0xcf => return Err(Error::Unsupported("only SOF0 baseline Huffman JPEG")),
                _ => return Err(Error::Invalid("unsupported marker")),
            }
        }
    }
}
fn be(bytes: &[u8], at: usize) -> Result<u16, Error> {
    let p = bytes.get(at..at + 2).ok_or(Error::Truncated)?;
    Ok(u16::from_be_bytes([p[0], p[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    const GRAY: &[u8] = include_bytes!("../../../tests/fixtures/hw-gray16.jpg");
    const COLOR: [&[u8]; 3] = [
        include_bytes!("../../../tests/fixtures/hw-color-0.jpg"),
        include_bytes!("../../../tests/fixtures/hw-color-1.jpg"),
        include_bytes!("../../../tests/fixtures/hw-color-2.jpg"),
    ];
    #[test]
    fn supported_sampling_and_sos_dma_origin() {
        let gray = Header::parse(GRAY).unwrap();
        assert_eq!(
            (
                gray.width,
                gray.height,
                gray.components,
                gray.output_bytes()
            ),
            (16, 16, 1, 256)
        );
        for (i, data) in COLOR.iter().enumerate() {
            let h = Header::parse(data).unwrap();
            assert_eq!((h.width, h.height, h.components), (32, 16, 3));
            assert_eq!(
                (h.component[0].h, h.component[0].v),
                [(1, 1), (2, 1), (2, 2)][i]
            );
            assert_eq!(&data[h.sos..h.sos + 2], &[255, 218]);
            assert_eq!(h.output_bytes(), 1024);
        }
    }
    #[test]
    fn rejects_truncated_header_with_apparent_eoi() {
        let h = Header::parse(COLOR[2]).unwrap();
        let mut p = [0u8; 2048];
        for cut in 0..h.sos + 2 {
            p[..cut].copy_from_slice(&COLOR[2][..cut]);
            p[cut..cut + 2].copy_from_slice(&[255, 217]);
            assert!(Header::parse(&p[..cut + 2]).is_err());
        }
    }
    #[test]
    fn mcu_padding_is_checked_before_output_allocation() {
        let mut p = [0u8; 2048];
        let n = COLOR[2].len();
        p[..n].copy_from_slice(COLOR[2]);
        let sof = p[..n].windows(2).position(|w| w == [255, 192]).unwrap();
        p[sof + 5..sof + 9].copy_from_slice(&[0, 24, 0, 24]);
        let h = Header::parse(&p[..n]).unwrap();
        assert_eq!((h.stride, h.rows, h.output_bytes()), (32, 32, 2048));
        p[sof + 7..sof + 9].copy_from_slice(&[255, 248]);
        assert!(Header::parse(&p[..n]).is_err());
    }
    #[test]
    fn rejects_progressive_zero_quantizer_and_oversubscribed_codes() {
        for marker in [0xc0, 0xdb, 0xc4] {
            let mut p = [0u8; 2048];
            let n = COLOR[2].len();
            p[..n].copy_from_slice(COLOR[2]);
            let at = p[..n].windows(2).position(|w| w == [255, marker]).unwrap();
            match marker {
                0xc0 => p[at + 1] = 0xc2,
                0xdb => p[at + 5] = 0,
                _ => p[at + 5] = 255,
            };
            assert!(Header::parse(&p[..n]).is_err());
        }
    }
}
