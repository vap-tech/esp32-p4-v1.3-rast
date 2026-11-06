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
/// Optional coarse profiler. Clock ticks may be cycles or another monotonic
/// wrapping u64 counter. No clock call is made in disabled mode.
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    clock: Option<fn() -> u64>,
    pub ticks: [u64; 5],
    pub calls: [u32; 5],
    pub mcus: u32,
    pub pixels: u32,
    pub blocks: u32,
    pub dc_only_blocks: u32,
    pub idct: super::idct::Kind,
    #[cfg(test)]
    pub block_observer: Option<fn(&[i32; 64])>,
}
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Stage {
    EntropyDequant,
    Idct,
    Color,
    Output,
    Header,
}
impl Profile {
    pub const fn disabled() -> Self {
        Self {
            clock: None,
            ticks: [0; 5],
            calls: [0; 5],
            mcus: 0,
            pixels: 0,
            blocks: 0,
            dc_only_blocks: 0,
            idct: super::idct::Kind::Matrix,
            #[cfg(test)]
            block_observer: None,
        }
    }
    pub fn new(clock: fn() -> u64) -> Self {
        Self {
            clock: Some(clock),
            ..Self::disabled()
        }
    }
    pub fn with_idct(mut self, kind: super::idct::Kind) -> Self {
        self.idct = kind;
        self
    }
    fn begin(&self) -> u64 {
        self.clock.map_or(0, |clock| clock())
    }
    fn end(&mut self, stage: Stage, start: u64) {
        if let Some(clock) = self.clock {
            self.ticks[stage as usize] =
                self.ticks[stage as usize].wrapping_add(clock().wrapping_sub(start));
            self.calls[stage as usize] += 1;
        }
    }
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
        profile: &mut Profile,
    ) -> Result<[u8; 64], Error<R::Error>> {
        let entropy_started = profile.begin();
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
        core::hint::black_box(&coefficients);
        profile.end(Stage::EntropyDequant, entropy_started);
        #[cfg(test)]
        if let Some(observer) = profile.block_observer {
            observer(&coefficients);
        }
        let idct_started = profile.begin();
        let (block, dc_only) = super::idct::transform(&coefficients, profile.idct);
        core::hint::black_box(&block);
        profile.end(Stage::Idct, idct_started);
        if profile.clock.is_some() {
            profile.blocks += 1;
            profile.dc_only_blocks += u32::from(dc_only);
        }
        Ok(block)
    }
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
    pixels: impl FnMut(usize, usize, &[u16]),
) -> Result<Info, Error<R::Error>> {
    decode_profiled(input, width, height, pixels, &mut Profile::disabled())
}
/// Same decoder and arithmetic with optional stage instrumentation. MCU RGB565
/// rows are assembled in 512 B of stack scratch before invoking output callbacks,
/// allowing one color/output measurement per MCU instead of per row/pixel.
#[inline(never)]
pub fn decode_profiled<R: Input>(
    input: &mut R,
    width: usize,
    height: usize,
    mut pixels: impl FnMut(usize, usize, &[u16]),
    profile: &mut Profile,
) -> Result<Info, Error<R::Error>> {
    let header_started = profile.begin();
    input.seek(0).map_err(Error::Io)?;
    let mut r = Reader::new(input);
    let mut decoder = Decoder::new();
    decoder.header(&mut r)?;
    profile.end(Stage::Header, header_started);
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
                        let block = decoder.block(&mut r, i, profile)?;
                        for y in 0..8 {
                            plane[(by * 8 + y) * c.h * 8 + bx * 8
                                ..(by * 8 + y) * c.h * 8 + bx * 8 + 8]
                                .copy_from_slice(&block[y * 8..y * 8 + 8]);
                        }
                    }
                }
            }
            let color_started = profile.begin();
            let lines = mh.min(height - my * mh);
            let n = mw.min(width - mx * mw);
            let mut rgb = [[0u16; 16]; 16];
            for (y, row) in rgb[..lines].iter_mut().enumerate() {
                for (x, pixel) in row[..n].iter_mut().enumerate() {
                    let chroma = (y / info.v_sampling) * 8 + x / info.h_sampling;
                    *pixel = rgb565(planes[0][y * mw + x], planes[1][chroma], planes[2][chroma]);
                }
            }
            core::hint::black_box(&rgb);
            profile.end(Stage::Color, color_started);
            let output_started = profile.begin();
            for (y, row) in rgb[..lines].iter().enumerate() {
                pixels(mx * mw, my * mh + y, &row[..n]);
            }
            profile.end(Stage::Output, output_started);
            if profile.clock.is_some() {
                profile.mcus += 1;
                profile.pixels += (n * lines) as u32;
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
        assert_eq!(super::super::idct::matrix(&c), [128; 64]);
        c[0] = 80;
        assert_eq!(super::super::idct::matrix(&c), [138; 64]);
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
    #[test]
    fn profiling_preserves_pixels_and_counts_blocks_mcus_and_outputs() {
        use core::sync::atomic::{AtomicU64, Ordering};
        static TICKS: AtomicU64 = AtomicU64::new(0);
        fn clock() -> u64 {
            TICKS.fetch_add(10, Ordering::Relaxed)
        }
        let data = include_bytes!("../../tests/data/gradient420.jpg");
        let mut plain = [0u16; 17 * 19];
        decode(&mut Bytes { data, pos: 0 }, 17, 19, |x, y, p| {
            plain[y * 17 + x..y * 17 + x + p.len()].copy_from_slice(p)
        })
        .unwrap();
        let mut measured = [0u16; 17 * 19];
        let mut profile = Profile::new(clock);
        decode_profiled(
            &mut Bytes { data, pos: 0 },
            17,
            19,
            |x, y, p| measured[y * 17 + x..y * 17 + x + p.len()].copy_from_slice(p),
            &mut profile,
        )
        .unwrap();
        assert_eq!(plain, measured);
        assert_eq!(profile.calls, [24, 24, 4, 4, 1]);
        assert_eq!((profile.mcus, profile.pixels), (4, 323));
        assert_eq!(profile.ticks, [240, 240, 40, 40, 10]);
    }
    #[test]
    #[ignore = "set P4_JPEG_CHECK_FILE; writes host comparison artifacts under target/idct-reference"]
    fn compare_idct_external_file() {
        extern crate std;
        use std::{cell::RefCell, vec::Vec};
        std::thread_local! {static BLOCKS:RefCell<Vec<[i32;64]>>=const {RefCell::new(Vec::new())};}
        fn observe(c: &[i32; 64]) {
            BLOCKS.with(|blocks| blocks.borrow_mut().push(*c));
        }
        let data = std::fs::read(std::env::var("P4_JPEG_CHECK_FILE").unwrap()).unwrap();
        let out_dir = std::path::PathBuf::from(
            std::env::var("P4_IDCT_OUT_DIR").unwrap_or_else(|_| "target/idct-reference".into()),
        );
        std::fs::create_dir_all(&out_dir).unwrap();
        let mut matrix_pixels = std::vec![0u16;1024*600];
        let mut profile = Profile::disabled();
        profile.block_observer = Some(observe);
        decode_profiled(
            &mut Bytes {
                data: &data,
                pos: 0,
            },
            1024,
            600,
            |x, y, p| matrix_pixels[y * 1024 + x..y * 1024 + x + p.len()].copy_from_slice(p),
            &mut profile,
        )
        .unwrap();
        let blocks = BLOCKS.with(|b| core::mem::take(&mut *b.borrow_mut()));
        let mut coefficients = Vec::new();
        let mut matrix_blocks = Vec::new();
        let mut candidate_blocks = Vec::new();
        let (mut dc_only, mut changed, mut changed_blocks, mut max_error, mut sum_error) =
            (0usize, 0usize, 0usize, 0u8, 0u64);
        for c in &blocks {
            let (a, dc) = super::super::idct::transform(c, super::super::idct::Kind::Matrix);
            let b = super::super::idct::transform(c, super::super::idct::Kind::Loeffler).0;
            dc_only += usize::from(dc);
            changed_blocks += usize::from(a != b);
            for (&a, &b) in a.iter().zip(&b) {
                let error = a.abs_diff(b);
                changed += usize::from(error != 0);
                max_error = max_error.max(error);
                sum_error += u64::from(error);
            }
            for value in c {
                coefficients.extend_from_slice(&value.to_le_bytes());
            }
            matrix_blocks.extend_from_slice(&a);
            candidate_blocks.extend_from_slice(&b);
        }
        std::fs::write(out_dir.join("coefficients.i32le"), coefficients).unwrap();
        std::fs::write(out_dir.join("matrix-blocks.u8"), matrix_blocks).unwrap();
        std::fs::write(out_dir.join("loeffler-blocks.u8"), candidate_blocks).unwrap();
        let mut candidate_pixels = std::vec![0u16;1024*600];
        let mut profile = Profile::disabled().with_idct(super::super::idct::Kind::Loeffler);
        decode_profiled(
            &mut Bytes {
                data: &data,
                pos: 0,
            },
            1024,
            600,
            |x, y, p| candidate_pixels[y * 1024 + x..y * 1024 + x + p.len()].copy_from_slice(p),
            &mut profile,
        )
        .unwrap();
        let (mut rgb_changed, mut rgb_max) = (0usize, [0u8; 3]);
        for (&a, &b) in matrix_pixels.iter().zip(&candidate_pixels) {
            rgb_changed += usize::from(a != b);
            for (i, shift, mask) in [(0, 11, 31), (1, 5, 63), (2, 0, 31)] {
                rgb_max[i] = rgb_max[i]
                    .max((((a >> shift) & mask) as u8).abs_diff(((b >> shift) & mask) as u8));
            }
        }
        for (name, pixels) in [
            ("matrix.rgb565", matrix_pixels),
            ("loeffler.rgb565", candidate_pixels),
        ] {
            let bytes: Vec<_> = pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
            std::fs::write(out_dir.join(name), bytes).unwrap();
        }
        std::println!("IDCT HOST blocks={} dc_only={} changed_blocks={} sample_count={} changed_samples={} max_error={} sum_abs_error={}",blocks.len(),dc_only,changed_blocks,blocks.len()*64,changed,max_error,sum_error);
        std::println!(
            "IDCT HOST rgb565_changed_pixels={} rgb565_max_channel_steps={:?}",
            rgb_changed,
            rgb_max
        );
        // Independent C/UBSan oracle also receives boundary and adversarial
        // vectors. These are overflow tests, not a <=1-error promise for arbitrary
        // non-photographic coefficient arrays.
        let mut stress = Vec::new();
        for sign in [-1, 1] {
            for index in 0..64 {
                let mut c = [0; 64];
                c[index] = sign * 522240;
                stress.push(c);
            }
            stress.push([sign * 522240; 64]);
            stress.push(core::array::from_fn(|i| {
                if i % 2 == 0 {
                    522240
                } else {
                    -522240
                }
            }));
        }
        let mut seed = 0x13579bdfu32;
        for _ in 0..1024 {
            stress.push(core::array::from_fn(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let limit = if i == 0 { 522240 } else { 260865 };
                (seed % (limit * 2 + 1)) as i32 - limit as i32
            }));
        }
        let mut raw = Vec::new();
        let mut expected = Vec::new();
        for c in &stress {
            for v in c {
                raw.extend_from_slice(&v.to_le_bytes());
            }
            expected.extend_from_slice(
                &super::super::idct::transform(c, super::super::idct::Kind::Loeffler).0,
            );
        }
        std::fs::write(out_dir.join("stress-coefficients.i32le"), raw).unwrap();
        std::fs::write(out_dir.join("stress-loeffler.u8"), expected).unwrap();
        std::println!("IDCT HOST overflow stress blocks={}", stress.len());
        assert_eq!(blocks.len(), 14592);
        assert!(max_error <= 1);
        assert!(rgb_max.iter().all(|&v| v <= 1));
    }
    #[test]
    fn loeffler_sampling_and_color_match_independent_small_fixtures() {
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
            let mut profile = Profile::disabled().with_idct(super::super::idct::Kind::Loeffler);
            decode_profiled(
                &mut Bytes { data, pos: 0 },
                17,
                19,
                |x, y, p| actual[y * 17 + x..y * 17 + x + p.len()].copy_from_slice(p),
                &mut profile,
            )
            .unwrap();
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
                    assert!(got[c].abs_diff(expected[c]) <= 1);
                }
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
