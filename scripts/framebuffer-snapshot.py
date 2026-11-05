#!/usr/bin/env python3
"""Save firmware W full screenshot as PNG, using only Python's standard library.

By default pauses camera DMA, copies the framebuffer, resumes and dumps the copy.
Use --live for the old 128x75 thumbnail. Do not run a monitor
concurrently. --from-log reconstructs the last thumbnail without using USB.
"""
import argparse
from pathlib import Path
import re
import struct
import subprocess
import sys
import zlib


def encode(dump, scale):
    full = 'FB_FULL 1024 600' in dump
    width, height = (1024, 600) if full else (128, 75)
    section = dump.rsplit(f'FB_FULL {width} {height}' if full else 'FB_THUMB 128 75', 1)[-1]
    rows = re.findall(rf'^FBROW ([0-9a-f]{{{width * 4}}})\r?$', section, re.M)
    if len(rows) != height:
        raise ValueError(f'Expected {height} complete rows, got {len(rows)}')
    scanlines = bytearray()
    for row in rows:
        line = bytearray([0])
        for offset in range(0, width * 4, 4):
            rgb = int(row[offset:offset + 4], 16)
            color = bytes([((rgb >> 11) & 31) * 255 // 31,
                           ((rgb >> 5) & 63) * 255 // 63,
                           (rgb & 31) * 255 // 31])
            line.extend(color * scale)
        scanlines.extend(line * scale)

    def chunk(kind, data):
        return (struct.pack('>I', len(data)) + kind + data
                + struct.pack('>I', zlib.crc32(kind + data)))

    return (b'\x89PNG\r\n\x1a\n'
            + chunk(b'IHDR', struct.pack('>IIBBBBB', width * scale, height * scale, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(bytes(scanlines))) + chunk(b'IEND', b''))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--port', default='/dev/ttyACM0')
    parser.add_argument('--scale', type=int, default=1, choices=range(1, 17))
    parser.add_argument('--live', action='store_true')
    parser.add_argument('--from-log', type=Path)
    args = parser.parse_args()
    if args.from_log:
        dump = args.from_log.read_text()
    else:
        commands = ['H'] if args.live else ['W']
        result = subprocess.run([sys.executable, str(Path(__file__).with_name('display-control.py')),
                                 '--port', args.port, '--hold', '0.1', *commands],
                                capture_output=True, text=True, timeout=200)
        if result.returncode:
            raise RuntimeError(result.stderr + result.stdout[-2000:])
        dump = result.stdout
    png = encode(dump, args.scale)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(png)
    print(f'Saved {args.output}: full resolution' if 'FB_FULL' in dump else f'Saved {args.output}: thumbnail')


if __name__ == '__main__':
    main()
