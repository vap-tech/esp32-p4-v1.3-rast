//! JPEG inverse transforms over already-dequantized coefficients.
//! Matrix is the original local Q14 reference; Loeffler is an isolated IJG port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Matrix,
    Loeffler,
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
pub fn matrix(c: &[i32; 64]) -> [u8; 64] {
    transform(c, Kind::Matrix).0
}
#[inline(never)]
fn matrix_dense(c: &[i32; 64]) -> [u8; 64] {
    let mut out = [0u8; 64];
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

/// Shared exact DC-only shortcut. Both algorithms retain their original clipping.
pub fn transform(c: &[i32; 64], kind: Kind) -> ([u8; 64], bool) {
    let dc_only = c[1..].iter().all(|&v| v == 0);
    if dc_only {
        return ([(((c[0] + 4) >> 3) + 128).clamp(0, 255) as u8; 64], true);
    }
    (
        match kind {
            Kind::Matrix => matrix_dense(c),
            Kind::Loeffler => super::loeffler::transform(c),
        },
        false,
    )
}
