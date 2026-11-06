#!/usr/bin/env python3
"""Host-only comparison with pinned libjpeg-turbo C IDCT; no firmware dependency.

Downloads an upstream source into ignored target/, extracts its 8x8 routine,
uses dequantized i32 input / wide products / saturating output like our Rust
adaptation, and compares every component sample under the C UB sanitizer.
"""
import argparse
import hashlib
import os
from pathlib import Path
import re
import subprocess
import urllib.request

COMMIT = '29e4d9b0178f08158fc72c6068f086c5ba27b772'
SHA256 = '4ba1bd508d16b3b0d5849a7e7b9dcab79c24cd1701edb054605ef47f5ef8f239'
ROOT = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--jpeg', type=Path, default=ROOT / 'target/sd-media/HOME.JPG')
args = parser.parse_args()
work = ROOT / 'target/idct-reference'
work.mkdir(parents=True, exist_ok=True)
source = work / 'jidctint.c'
if not source.exists():
    source.write_bytes(urllib.request.urlopen(
        f'https://raw.githubusercontent.com/libjpeg-turbo/libjpeg-turbo/{COMMIT}/src/jidctint.c', timeout=30).read())
if hashlib.sha256(source.read_bytes()).hexdigest() != SHA256:
    raise RuntimeError('Reference source checksum differs from pinned commit')
src = source.read_text()
start = src.index('_jpeg_idct_islow(')
brace = src.index('{', start)
depth = 1
end = brace + 1
while depth:
    depth += (src[end] == '{') - (src[end] == '}')
    end += 1
body = src[start:end]
body = re.sub(r'  _JSAMPLE \*range_limit = IDCT_range_limit\(cinfo\);\n', '', body)
while 'range_limit[' in body:
    a = body.index('range_limit[')
    b = a + len('range_limit[')
    depth = 1
    while depth:
        depth += (body[b] == '[') - (body[b] == ']')
        b += 1
    expr = re.sub(r'&\s*RANGE_MASK\s*$', '', body[a + len('range_limit['):b-1]).strip()
    body = body[:a] + 'clip(' + expr + ')' + body[b:]
constants = src[src.index('#if CONST_BITS == 13'):]
constants = constants[:constants.index('#else')]
constants = '\n'.join(line for line in constants.splitlines() if line.startswith('#define FIX_'))
header = src[:src.index('#define JPEG_INTERNALS')]
prelude = r'''
#include <stdint.h>
#include <stdio.h>
#define CONST_BITS 13
#define PASS1_BITS 2
#define DCTSIZE 8
#define DCTSIZE2 64
#define SHIFT_TEMPS
#define SCALING_FACTOR
#define DEQUANTIZE(coef, quant) ((int64_t)(coef))
#define MULTIPLY(a, b) ((int64_t)(a) * (int64_t)(b))
#define LEFT_SHIFT(a, b) ((int64_t)(a) * (INT64_C(1) << (b)))
#define DESCALE(a, b) (((int64_t)(a) + (INT64_C(1) << ((b)-1))) >> (b))
typedef int64_t JLONG;
typedef int32_t *JCOEFPTR;
typedef int32_t ISLOW_MULT_TYPE;
typedef uint8_t _JSAMPLE;
typedef uint8_t *_JSAMPROW;
typedef uint8_t **_JSAMPARRAY;
typedef unsigned JDIMENSION;
typedef void *j_decompress_ptr;
typedef struct { int32_t *dct_table; } jpeg_component_info;
static uint8_t clip(int64_t value) {
    value += 128;
    return value < 0 ? 0 : value > 255 ? 255 : (uint8_t)value;
}
'''
main = r'''
int main(void) {
    int32_t coeff[64], quant[64] = {0};
    uint8_t out[64], *rows[8];
    jpeg_component_info comp = {quant};
    for (int y=0;y<8;y++) rows[y]=out+y*8;
    size_t n;
    while ((n=fread(coeff,sizeof(int32_t),64,stdin))) {
        if (n != 64) return 2;
        _jpeg_idct_islow(NULL,&comp,coeff,rows,0);
        if (fwrite(out,1,64,stdout) != 64) return 3;
    }
    return ferror(stdin) ? 4 : 0;
}
'''
(work / 'oracle.c').write_text(header + '\n' + prelude + constants + '\nvoid ' + body + '\n' + main)
subprocess.run(['cc', '-O2', '-fsanitize=undefined', '-fno-sanitize-recover=all',
                str(work / 'oracle.c'), '-o', str(work / 'oracle')], check=True)
env = os.environ.copy()
env['P4_JPEG_CHECK_FILE'] = str(args.jpeg.resolve())
env['P4_IDCT_OUT_DIR'] = str(work)
subprocess.run(['cargo', 'test', '--lib', '--target', 'x86_64-unknown-linux-gnu',
                'compare_idct_external_file', '--', '--ignored', '--nocapture'],
               cwd=ROOT, env=env, check=True)
with (work / 'coefficients.i32le').open('rb') as inp, (work / 'c-blocks.u8').open('wb') as out:
    subprocess.run([str(work / 'oracle')], stdin=inp, stdout=out, check=True)
expected = (work / 'c-blocks.u8').read_bytes()
actual = (work / 'loeffler-blocks.u8').read_bytes()
if expected != actual:
    raise RuntimeError(f'C/Rust IDCT mismatch: {sum(a != b for a,b in zip(expected,actual))} samples')
print(f'Pinned C / Rust Loeffler: all {len(actual)} samples identical; UBSan clean.')

with (work / 'stress-coefficients.i32le').open('rb') as inp, (work / 'stress-c.u8').open('wb') as out:
    subprocess.run([str(work / 'oracle')], stdin=inp, stdout=out, check=True)
expected = (work / 'stress-c.u8').read_bytes()
actual = (work / 'stress-loeffler.u8').read_bytes()
if expected != actual:
    raise RuntimeError('Boundary C/Rust mismatch')
print(f'C / Rust boundary stress: {len(actual)//64} blocks identical; UBSan clean.')
