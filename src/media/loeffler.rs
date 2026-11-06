// Rust adaptation of libjpeg-turbo src/jidctint.c, 8-bit 8x8 _jpeg_idct_islow.
// Original: Copyright (C) 1991-1998, Thomas G. Lane.
// Modification developed 2002-2018 by Guido Vollbeding.
// libjpeg-turbo: Copyright (C) 2015, 2020, 2022, 2026, D. R. Commander.
// Rust adaptation: Copyright (C) 2026 ESP32-P4 project contributors.
// Conditions: licenses/libjpeg-turbo-README.ijg.txt (IJG license).
// Pinned upstream: 29e4d9b0178f08158fc72c6068f086c5ba27b772.
// Changes: dequant already done by caller; wide i64 products/sums; explicit
// saturating output instead of IJG's masked range table; no libjpeg runtime.

const CONST_BITS: u32 = 13;
const PASS1_BITS: u32 = 2;
fn descale(value: i64, bits: u32) -> i64 {
    (value + (1i64 << (bits - 1))) >> bits
}
fn mul(value: i32, constant: i32) -> i64 {
    i64::from(value) * i64::from(constant)
}

/// Loeffler alternate 12-multiply, 32-add 1D factorization, output scaled by 2^13.
/// Caller bounds inputs: first pass <=522240; second pass <=45_445_846.
/// Thus all pre-multiply i32 sums (at most 4 inputs) fit; products/sums use i64.
#[inline(never)]
fn kernel(v: [i32; 8]) -> [i64; 8] {
    let z1 = mul(v[2] + v[6], 4433);
    let tmp2 = z1 - mul(v[6], 15137);
    let tmp3 = z1 + mul(v[2], 6270);
    let tmp0 = i64::from(v[0] + v[4]) << CONST_BITS;
    let tmp1 = i64::from(v[0] - v[4]) << CONST_BITS;
    let tmp10 = tmp0 + tmp3;
    let tmp13 = tmp0 - tmp3;
    let tmp11 = tmp1 + tmp2;
    let tmp12 = tmp1 - tmp2;
    let (a, b, c, d) = (v[7], v[5], v[3], v[1]);
    let z5 = mul((a + c) + (b + d), 9633);
    let z1 = -mul(a + d, 7373);
    let z2 = -mul(b + c, 20995);
    let z3 = -mul(a + c, 16069) + z5;
    let z4 = -mul(b + d, 3196) + z5;
    let tmp0 = mul(a, 2446) + z1 + z3;
    let tmp1 = mul(b, 16819) + z2 + z4;
    let tmp2 = mul(c, 25172) + z2 + z3;
    let tmp3 = mul(d, 12299) + z1 + z4;
    [
        tmp10 + tmp3,
        tmp11 + tmp2,
        tmp12 + tmp1,
        tmp13 + tmp0,
        tmp13 - tmp0,
        tmp12 - tmp1,
        tmp11 - tmp2,
        tmp10 - tmp3,
    ]
}
#[inline(never)]
pub(super) fn transform(c: &[i32; 64]) -> [u8; 64] {
    let mut workspace = [0i32; 64];
    for x in 0..8 {
        if (1..8).all(|y| c[y * 8 + x] == 0) {
            for y in 0..8 {
                workspace[y * 8 + x] = c[x] * (1 << PASS1_BITS);
            }
        } else {
            let column = core::array::from_fn(|y| c[y * 8 + x]);
            let values = kernel(column);
            for y in 0..8 {
                let value = descale(values[y], CONST_BITS - PASS1_BITS);
                debug_assert!(i32::try_from(value).is_ok());
                workspace[y * 8 + x] = value as i32;
            }
        }
    }
    let mut out = [0u8; 64];
    for y in 0..8 {
        let row: &[i32; 8] = workspace[y * 8..y * 8 + 8].try_into().unwrap();
        if row[1..].iter().all(|&v| v == 0) {
            out[y * 8..y * 8 + 8]
                .fill((descale(i64::from(row[0]), PASS1_BITS + 3) + 128).clamp(0, 255) as u8);
        } else {
            let values = kernel(*row);
            for x in 0..8 {
                out[y * 8 + x] =
                    (descale(values[x], CONST_BITS + PASS1_BITS + 3) + 128).clamp(0, 255) as u8;
            }
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dc_and_single_ac_have_expected_symmetry() {
        let mut c = [0; 64];
        c[0] = 80;
        assert_eq!(transform(&c), [138; 64]);
        c[0] = 0;
        c[1] = 40;
        let out = transform(&c);
        for y in 0..8 {
            assert_eq!(&out[y * 8..y * 8 + 8], &out[..8]);
        }
        for x in 0..8 {
            assert!((i32::from(out[x]) + i32::from(out[7 - x]) - 256).abs() <= 1);
        }
    }
    #[test]
    fn decoded_coefficient_extremes_and_sparse_blocks_do_not_overflow() {
        // Entire syntactically accepted coefficient domain, not just photographic input.
        for sign in [-1, 1] {
            for index in 0..64 {
                let mut c = [0; 64];
                c[index] = sign * 522240;
                let _ = transform(&c);
            }
            let _ = transform(&[sign * 522240; 64]);
        }
        let mut seed = 0x13579bdfu32;
        for _ in 0..1024 {
            let c = core::array::from_fn(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let limit = if i == 0 { 522240 } else { 260865 };
                (seed % (limit * 2 + 1)) as i32 - limit as i32
            });
            let _ = transform(&c);
        }
    }
}
