//! Baseline sequential 8-bit YCbCr JPEG -> RGB565, streaming, no allocator.
//! Supports 4:4:4 / 4:2:2 / 4:2:0, explicit DQT/DHT and restart intervals.
//! Progressive, arithmetic, CMYK and multi-scan JPEG are rejected.
use super::{Error, Input};
const ZIG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];
#[derive(Clone, Copy)]
struct Huffman {
    counts: [u8; 16],
    symbols: [u8; 256],
    valid: bool,
}
impl Huffman {
    const EMPTY: Self = Self {
        counts: [0; 16],
        symbols: [0; 256],
        valid: false,
    };
}
#[derive(Clone, Copy)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    q: usize,
    dc: usize,
    ac: usize,
    predictor: i32,
}
impl Component {
    const EMPTY: Self = Self {
        id: 0,
        h: 0,
        v: 0,
        q: 0,
        dc: 0,
        ac: 0,
        predictor: 0,
    };
}
#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub width: usize,
    pub height: usize,
    pub h_sampling: usize,
    pub v_sampling: usize,
}
struct Reader<'a, R> {
    input: &'a mut R,
    buffer: [u8; 1024],
    pos: usize,
    len: usize,
    offset: u32,
    bits: u32,
    available: u8,
}
impl<'a, R: Input> Reader<'a, R> {
    fn new(input: &'a mut R) -> Self {
        Self {
            input,
            buffer: [0; 1024],
            pos: 0,
            len: 0,
            offset: 0,
            bits: 0,
            available: 0,
        }
    }
    fn byte(&mut self) -> Result<u8, Error<R::Error>> {
        if self.pos == self.len {
            self.len = self.input.read(&mut self.buffer).map_err(Error::Io)?;
            self.pos = 0;
            if self.len == 0 {
                return Err(Error::Invalid("truncated JPEG"));
            }
        }
        let b = self.buffer[self.pos];
        self.pos += 1;
        self.offset += 1;
        Ok(b)
    }
    fn word(&mut self) -> Result<u16, Error<R::Error>> {
        Ok((u16::from(self.byte()?) << 8) | u16::from(self.byte()?))
    }
    fn marker(&mut self) -> Result<u8, Error<R::Error>> {
        if self.byte()? != 255 {
            return Err(Error::Invalid("JPEG marker prefix"));
        }
        loop {
            let b = self.byte()?;
            if b != 255 {
                if b == 0 {
                    return Err(Error::Invalid("JPEG marker zero"));
                }
                return Ok(b);
            }
        }
    }
    fn get(&mut self, n: u8) -> Result<u32, Error<R::Error>> {
        while self.available < n {
            let b = self.byte()?;
            if b == 255 && self.byte()? != 0 {
                return Err(Error::Invalid("unexpected JPEG entropy marker"));
            }
            self.bits = (self.bits << 8) | u32::from(b);
            self.available += 8;
        }
        self.available -= n;
        Ok((self.bits >> self.available) & ((1 << n) - 1))
    }
    fn symbol(&mut self, h: &Huffman) -> Result<u8, Error<R::Error>> {
        if !h.valid {
            return Err(Error::Invalid("missing JPEG Huffman table"));
        }
        let (mut code, mut first, mut offset) = (0u32, 0u32, 0usize);
        for &count in &h.counts {
            code = (code << 1) | self.get(1)?;
            if code >= first && code - first < u32::from(count) {
                return Ok(h.symbols[offset + (code - first) as usize]);
            }
            offset += usize::from(count);
            first = (first + u32::from(count)) << 1;
        }
        Err(Error::Invalid("invalid JPEG Huffman code"))
    }
    fn signed(&mut self, n: u8) -> Result<i32, Error<R::Error>> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.get(n)? as i32;
        Ok(if v < (1 << (n - 1)) {
            v + 1 - (1 << n)
        } else {
            v
        })
    }
}
struct Decoder {
    quant: [[u16; 64]; 4],
    q_valid: [bool; 4],
    huff: [[Huffman; 2]; 2],
    components: [Component; 3],
    info: Info,
    restart: u16,
    frame: bool,
}
impl Decoder {
    fn new() -> Self {
        Self {
            quant: [[0; 64]; 4],
            q_valid: [false; 4],
            huff: [[Huffman::EMPTY; 2]; 2],
            components: [Component::EMPTY; 3],
            info: Info {
                width: 0,
                height: 0,
                h_sampling: 0,
                v_sampling: 0,
            },
            restart: 0,
            frame: false,
        }
    }
    fn header<R: Input>(&mut self, r: &mut Reader<'_, R>) -> Result<(), Error<R::Error>> {
        if r.word()? != 0xffd8 {
            return Err(Error::Invalid("JPEG SOI"));
        }
        loop {
            let marker = r.marker()?;
            if matches!(marker, 0xd8 | 0xd9 | 0xd0..=0xd7 | 0x01) {
                return Err(Error::Invalid("unexpected JPEG marker"));
            }
            let len = r.word()?;
            if len < 2 {
                return Err(Error::Invalid("JPEG segment length"));
            }
            let end = r
                .offset
                .checked_add(u32::from(len) - 2)
                .filter(|&end| end <= r.input.len())
                .ok_or(Error::Invalid("JPEG segment bounds"))?;
            match marker {
                0xdb => {
                    while r.offset < end {
                        let table = r.byte()?;
                        let id = usize::from(table & 15);
                        if table >> 4 != 0 || id >= 4 {
                            return Err(Error::Unsupported("JPEG quantization precision/table"));
                        }
                        if end - r.offset < 64 {
                            return Err(Error::Invalid("JPEG DQT size"));
                        }
                        for &at in &ZIG {
                            let q = r.byte()?;
                            if q == 0 {
                                return Err(Error::Invalid("zero JPEG quantizer"));
                            }
                            self.quant[id][at] = u16::from(q);
                        }
                        self.q_valid[id] = true;
                    }
                }
                0xc4 => {
                    while r.offset < end {
                        let table = r.byte()?;
                        let (class, id) = (usize::from(table >> 4), usize::from(table & 15));
                        if class > 1 || id > 1 {
                            return Err(Error::Unsupported("JPEG Huffman table ID"));
                        }
                        if end - r.offset < 16 {
                            return Err(Error::Invalid("JPEG DHT size"));
                        }
                        let h = &mut self.huff[class][id];
                        let (mut total, mut space) = (0usize, 1i32);
                        for count in &mut h.counts {
                            *count = r.byte()?;
                            total += usize::from(*count);
                            space = space * 2 - i32::from(*count);
                            if space < 0 {
                                return Err(Error::Invalid("oversubscribed Huffman table"));
                            }
                        }
                        if total == 0 || total > 256 || (end - r.offset) < total as u32 {
                            return Err(Error::Invalid("JPEG Huffman symbol count"));
                        }
                        for s in &mut h.symbols[..total] {
                            *s = r.byte()?;
                            if (class == 0 && *s > 11) || (class == 1 && *s & 15 > 10) {
                                return Err(Error::Invalid("JPEG Huffman symbol range"));
                            }
                        }
                        h.valid = true;
                    }
                }
                0xc0 => {
                    if self.frame || len != 17 || r.byte()? != 8 {
                        return Err(Error::Unsupported("JPEG frame/precision"));
                    }
                    let height = usize::from(r.word()?);
                    let width = usize::from(r.word()?);
                    if width == 0 || height == 0 || r.byte()? != 3 {
                        return Err(Error::Unsupported("JPEG requires three color components"));
                    }
                    for c in &mut self.components {
                        c.id = r.byte()?;
                        let hv = r.byte()?;
                        c.h = usize::from(hv >> 4);
                        c.v = usize::from(hv & 15);
                        c.q = usize::from(r.byte()?);
                        if c.q >= 4 {
                            return Err(Error::Invalid("JPEG quantization ID"));
                        }
                    }
                    let [y, cb, cr] = self.components;
                    if [y.id, cb.id, cr.id] != [1, 2, 3]
                        || !matches!((y.h, y.v), (1, 1) | (2, 1) | (2, 2))
                        || (cb.h, cb.v, cr.h, cr.v) != (1, 1, 1, 1)
                    {
                        return Err(Error::Unsupported("JPEG requires YCbCr 444/422/420"));
                    }
                    self.info = Info {
                        width,
                        height,
                        h_sampling: y.h,
                        v_sampling: y.v,
                    };
                    self.frame = true;
                }
                0xdd => {
                    if len != 4 {
                        return Err(Error::Invalid("JPEG DRI"));
                    }
                    self.restart = r.word()?;
                }
                0xda => {
                    if !self.frame || len != 12 || r.byte()? != 3 {
                        return Err(Error::Unsupported("JPEG multi-scan"));
                    }
                    for c in &mut self.components {
                        if r.byte()? != c.id {
                            return Err(Error::Unsupported("JPEG scan component order"));
                        }
                        let tables = r.byte()?;
                        c.dc = usize::from(tables >> 4);
                        c.ac = usize::from(tables & 15);
                        if c.dc > 1
                            || c.ac > 1
                            || !self.q_valid[c.q]
                            || !self.huff[0][c.dc].valid
                            || !self.huff[1][c.ac].valid
                        {
                            return Err(Error::Invalid("JPEG missing scan tables"));
                        }
                    }
                    if [r.byte()?, r.byte()?, r.byte()?] != [0, 63, 0] {
                        return Err(Error::Unsupported("JPEG spectral selection"));
                    }
                    return Ok(());
                }
                0xc1..=0xcf if marker != 0xc4 => {
                    return Err(Error::Unsupported("JPEG coding mode (baseline only)"))
                }
                0xe0..=0xef | 0xfe => {}
                _ => return Err(Error::Unsupported("JPEG marker")),
            }
            if r.offset > end {
                return Err(Error::Invalid("JPEG segment overrun"));
            }
            while r.offset < end {
                r.byte()?;
            }
        }
    }
    fn block<R: Input>(
        &mut self,
        r: &mut Reader<'_, R>,
        index: usize,
    ) -> Result<[u8; 64], Error<R::Error>> {
        let c = &mut self.components[index];
        let mut coefficients = [0i32; 64];
        let n = r.symbol(&self.huff[0][c.dc])?;
        c.predictor = c
            .predictor
            .checked_add(r.signed(n)?)
            .filter(|&v| (-2048..=2047).contains(&v))
            .ok_or(Error::Invalid("JPEG DC range"))?;
        coefficients[0] = c.predictor * i32::from(self.quant[c.q][0]);
        let mut k = 1;
        while k < 64 {
            let symbol = r.symbol(&self.huff[1][c.ac])?;
            if symbol == 0 {
                break;
            }
            if symbol == 0xf0 {
                k += 16;
                if k > 64 {
                    return Err(Error::Invalid("JPEG zero run"));
                }
                continue;
            }
            let n = symbol & 15;
            k += usize::from(symbol >> 4);
            if n == 0 || k >= 64 {
                return Err(Error::Invalid("JPEG AC run"));
            }
            coefficients[ZIG[k]] = r.signed(n)? * i32::from(self.quant[c.q][ZIG[k]]);
            k += 1;
        }
        Ok(idct(&coefficients))
    }
}

// C(u)*cos((2x+1)*u*pi/16), fixed point 14 bits.
const BASIS: [[i32; 8]; 8] = [
    [11585, 16069, 15137, 13623, 11585, 9102, 6270, 3196],
    [11585, 13623, 6270, -3196, -11585, -16069, -15137, -9102],
    [11585, 9102, -6270, -16069, -11585, 3196, 15137, 13623],
    [11585, 3196, -15137, -9102, 11585, 13623, -6270, -16069],
    [11585, -3196, -15137, 9102, 11585, -13623, -6270, 16069],
    [11585, -9102, -6270, 16069, -11585, -3196, 15137, -13623],
    [11585, -13623, 6270, 3196, -11585, 16069, -15137, 9102],
    [11585, -16069, 15137, -13623, 11585, -9102, 6270, -3196],
];
fn idct(c: &[i32; 64]) -> [u8; 64] {
    let mut out = [0u8; 64];
    if c[1..].iter().all(|&v| v == 0) {
        out.fill((((c[0] + 4) >> 3) + 128).clamp(0, 255) as u8);
        return out;
    }
    let mut temp = [0i32; 64];
    for v in 0..8 {
        for x in 0..8 {
            let mut sum = 0i64;
            for u in 0..8 {
                sum += i64::from(c[v * 8 + u]) * i64::from(BASIS[x][u]);
            }
            temp[v * 8 + x] = ((sum + 8192) >> 14) as i32;
        }
    }
    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0i64;
            for v in 0..8 {
                sum += i64::from(temp[v * 8 + x]) * i64::from(BASIS[y][v]);
            }
            out[y * 8 + x] = (((sum + 32768) >> 16) + 128).clamp(0, 255) as u8;
        }
    }
    out
}
fn rgb565(y: u8, cb: u8, cr: u8) -> u16 {
    let y = i32::from(y);
    let cb = i32::from(cb) - 128;
    let cr = i32::from(cr) - 128;
    let r = (y + ((91881 * cr + 32768) >> 16)).clamp(0, 255) as u16;
    let g = (y - ((22554 * cb + 46802 * cr + 32768) >> 16)).clamp(0, 255) as u16;
    let b = (y + ((116130 * cb + 32768) >> 16)).clamp(0, 255) as u16;
    ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)
}
/// Decode into short horizontal RGB565 runs. Dimensions must exactly match the
/// target: no scaling or framebuffer address assumptions in the media parser.
/// On malformed/truncated input the destination can contain a partial image.
#[inline(never)]
pub fn decode<R: Input>(
    input: &mut R,
    width: usize,
    height: usize,
    mut pixels: impl FnMut(usize, usize, &[u16]),
) -> Result<Info, Error<R::Error>> {
    input.seek(0).map_err(Error::Io)?;
    let mut r = Reader::new(input);
    let mut decoder = Decoder::new();
    decoder.header(&mut r)?;
    let info = decoder.info;
    if (info.width, info.height) != (width, height) {
        return Err(Error::Unsupported("JPEG dimensions must match framebuffer"));
    }
    let (mw, mh) = (info.h_sampling * 8, info.v_sampling * 8);
    let (cols, rows) = (width.div_ceil(mw), height.div_ceil(mh));
    let mut count = 0;
    let mut rst = 0;
    for my in 0..rows {
        for mx in 0..cols {
            if decoder.restart != 0 && count != 0 && count % usize::from(decoder.restart) == 0 {
                r.available = 0;
                if r.marker()? != 0xd0 + rst {
                    return Err(Error::Invalid("JPEG restart sequence"));
                }
                rst = (rst + 1) & 7;
                for c in &mut decoder.components {
                    c.predictor = 0;
                }
            }
            let mut planes = [[0u8; 256]; 3];
            for (i, plane) in planes.iter_mut().enumerate() {
                let c = decoder.components[i];
                for by in 0..c.v {
                    for bx in 0..c.h {
                        let block = decoder.block(&mut r, i)?;
                        for y in 0..8 {
                            plane[(by * 8 + y) * c.h * 8 + bx * 8
                                ..(by * 8 + y) * c.h * 8 + bx * 8 + 8]
                                .copy_from_slice(&block[y * 8..y * 8 + 8]);
                        }
                    }
                }
            }
            for y in 0..mh.min(height - my * mh) {
                let mut row = [0u16; 16];
                let n = mw.min(width - mx * mw);
                for (x, pixel) in row[..n].iter_mut().enumerate() {
                    let chroma = (y / info.v_sampling) * 8 + x / info.h_sampling;
                    *pixel = rgb565(planes[0][y * mw + x], planes[1][chroma], planes[2][chroma]);
                }
                pixels(mx * mw, my * mh + y, &row[..n]);
            }
            count += 1;
        }
    }
    r.available = 0;
    if r.marker()? != 0xd9 {
        return Err(Error::Invalid("JPEG EOI or multiple scans"));
    }
    Ok(info)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::tests::Bytes;
    #[test]
    fn idct_dc_and_primary_colors() {
        let mut c = [0; 64];
        assert_eq!(idct(&c), [128; 64]);
        c[0] = 80;
        assert_eq!(idct(&c), [138; 64]);
        assert_eq!(rgb565(0, 128, 128), 0);
        assert_eq!(rgb565(255, 128, 128), 0xffff);
        assert_eq!(rgb565(76, 85, 255), 0xf800);
    }
    #[test]
    fn baseline_sampling_and_partial_edge_mcus_match_reference() {
        for (data, reference) in [
            (
                include_bytes!("../../tests/data/gradient420.jpg").as_slice(),
                include_bytes!("../../tests/data/gradient420.rgb").as_slice(),
            ),
            (
                include_bytes!("../../tests/data/gradient422.jpg").as_slice(),
                include_bytes!("../../tests/data/gradient422.rgb").as_slice(),
            ),
            (
                include_bytes!("../../tests/data/gradient444.jpg").as_slice(),
                include_bytes!("../../tests/data/gradient444.rgb").as_slice(),
            ),
        ] {
            let mut actual = [0u16; 17 * 19];
            let mut written = [false; 17 * 19];
            decode(&mut Bytes { data, pos: 0 }, 17, 19, |x, y, pixels| {
                for (i, &p) in pixels.iter().enumerate() {
                    assert!(!written[y * 17 + x + i]);
                    written[y * 17 + x + i] = true;
                    actual[y * 17 + x + i] = p;
                }
            })
            .unwrap();
            assert!(written.iter().all(|&v| v));
            for (i, &pixel) in actual.iter().enumerate() {
                let expected = [
                    reference[i * 3] >> 3,
                    reference[i * 3 + 1] >> 2,
                    reference[i * 3 + 2] >> 3,
                ];
                let got = [
                    (pixel >> 11) as u8,
                    ((pixel >> 5) & 63) as u8,
                    (pixel & 31) as u8,
                ];
                for c in 0..3 {
                    assert!(
                        got[c].abs_diff(expected[c]) <= 1,
                        "pixel {i} channel {c}: {got:?} vs {expected:?}"
                    );
                }
            }
            for cut in [0, 2, 20, data.len() / 2, data.len() - 2] {
                assert!(decode(
                    &mut Bytes {
                        data: &data[..cut],
                        pos: 0
                    },
                    17,
                    19,
                    |_, _, _| {}
                )
                .is_err());
            }
        }
    }
    /// Host diagnostic for an actual card asset, kept out of ordinary unit tests.
    #[test]
    #[ignore = "set P4_JPEG_CHECK_FILE and optionally P4_JPEG_CHECK_OUT"]
    fn decode_external_file() {
        extern crate std;
        let data = std::fs::read(std::env::var("P4_JPEG_CHECK_FILE").unwrap()).unwrap();
        let mut out = std::vec![0u8;1024*600*2];
        decode(
            &mut Bytes {
                data: &data,
                pos: 0,
            },
            1024,
            600,
            |x, y, pixels| {
                for (i, p) in pixels.iter().enumerate() {
                    out[(y * 1024 + x + i) * 2..(y * 1024 + x + i) * 2 + 2]
                        .copy_from_slice(&p.to_le_bytes());
                }
            },
        )
        .unwrap();
        if let Ok(path) = std::env::var("P4_JPEG_CHECK_OUT") {
            std::fs::write(path, &out).unwrap();
        }
    }
    #[test]
    fn rejects_invalid_and_progressive() {
        for data in [
            b"garbage".as_slice(),
            b"\xff\xd8\xff\xc2\x00\x02",
            b"\xff\xd8\xff\xdb\x00\x01",
        ] {
            assert!(
                decode(&mut Bytes { data, pos: 0 }, 16, 16, |_, _, _| panic!(
                    "unexpected pixels"
                ))
                .is_err()
            );
        }
    }
}
