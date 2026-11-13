#!/usr/bin/env python3
"""Split espflash's RAM image into ROM bootstrap and CRC-checked SRAM payload.

Output is flashed at 0x2000. The fixed payload manifest lives at Flash 0x40000;
ROM receives only SPM segments, and boot.S loads the upper image afterwards.
No ELF/Python packages or ESP-IDF runtime are required.
"""
import hashlib
import gzip
from pathlib import Path
import struct
import sys
import zlib

FLASH_BASE = 0x2000
MANIFEST = 0x40000
PAYLOAD = MANIFEST + 64
MAGIC = b'P4UPPER1'
BINDING = struct.pack('<8sII', b'P4BIND01', 0xc0dec0de, 0x3f213f21)


def parse_rom(image):
    if len(image) < 24 or image[0] != 0xe9 or not 1 <= image[1] <= 16 or image[23] not in (0, 1):
        raise ValueError('invalid Espressif image header')
    segments = []
    pos = 24
    checksum = 0xef
    for _ in range(image[1]):
        if pos + 8 > len(image):
            raise ValueError('truncated segment header')
        address, length = struct.unpack_from('<II', image, pos)
        pos += 8
        if not length or length % 4 or pos + length > len(image):
            raise ValueError('invalid segment length')
        data = image[pos:pos + length]
        segments.append((address, data))
        for value in data:
            checksum ^= value
        pos += length
    check_at = pos | 15
    end = check_at + 1
    if check_at >= len(image) or image[check_at] != checksum:
        raise ValueError('ROM image checksum mismatch')
    if image[23]:
        if image[end:end + 32] != hashlib.sha256(image[:end]).digest():
            raise ValueError('ROM image SHA256 mismatch')
        end += 32
    if end != len(image):
        raise ValueError('unexpected bytes after ROM image')
    return image[:24], segments


def make_rom(header, segments):
    out = bytearray(header)
    out[1] = len(segments)
    checksum = 0xef
    for address, data in segments:
        out.extend(struct.pack('<II', address, len(data)))
        out.extend(data)
        for value in data:
            checksum ^= value
    out.extend(bytes((-len(out) - 1) % 16))
    out.append(checksum)
    if header[23]:
        out.extend(hashlib.sha256(out).digest())
    return bytes(out)


ASSET = 0x100000


def pack(image, html=None):
    header, segments = parse_rom(image)
    # Round-trip an actual espflash image before changing its segment list.
    if make_rom(header, segments) != image:
        raise ValueError('unsupported ROM padding/layout')
    resident = []
    upper = []
    for address, data in segments:
        if 0x30100000 <= address and address + len(data) <= 0x30102000:
            resident.append((address, data))
        elif 0x4ff40000 <= address and address + len(data) <= 0x4ffb0000:
            upper.append((address, data))
        else:
            raise ValueError(f'unexpected load region {address:#x}')
    if len(resident) != 1 or len(upper) != 1:
        raise ValueError('expected one SPM bootstrap and one upper payload')
    entry = struct.unpack_from('<I', header, 4)[0]
    if not resident[0][0] <= entry < resident[0][0] + len(resident[0][1]):
        raise ValueError('entry is outside the resident bootstrap')
    address, data = upper[0]
    if PAYLOAD + len(data) > 16 * 1024 * 1024:
        raise ValueError('payload exceeds Flash')
    crc = zlib.crc32(data)
    spm_address, spm = resident[0]
    if spm.count(BINDING) != 1:
        raise ValueError('missing/ambiguous bootstrap payload binding slot')
    at = spm.index(BINDING) + 8
    spm = spm[:at] + struct.pack('<II', crc, crc ^ 0xffffffff) + spm[at + 8:]
    rom = make_rom(header, [(spm_address, spm)])
    if len(rom) > MANIFEST - FLASH_BASE:
        raise ValueError('bootstrap overlaps payload manifest')
    manifest = struct.pack('<8s6I32s', MAGIC, 1, address, len(data),
                           crc, PAYLOAD, 0, bytes(32))
    assert len(manifest) == 64
    result = rom + b'\xff' * (MANIFEST - FLASH_BASE - len(rom)) + manifest + data
    if html is not None:
        asset = gzip.compress(html, compresslevel=9, mtime=0)
        if len(result) > ASSET - FLASH_BASE or len(asset) > 65536:
            raise ValueError('runtime overlaps asset slot or asset exceeds 64 KiB')
        result += b'\xff' * (ASSET - FLASH_BASE - len(result))
        result += struct.pack('<8sII', b'P4WEB001', len(asset), zlib.crc32(asset))
        result += asset + bytes(-len(asset) % 4)
    return result


if __name__ == '__main__':
    if len(sys.argv) not in (3, 4):
        raise SystemExit('usage: pack-image.py INPUT.bin OUTPUT.bin [INDEX.html]')
    result = pack(Path(sys.argv[1]).read_bytes(), Path(sys.argv[3]).read_bytes() if len(sys.argv) == 4 else None)
    Path(sys.argv[2]).write_bytes(result)
    fields = struct.unpack_from('<8s6I', result, MANIFEST - FLASH_BASE)
    print(f'SPM bootstrap + upper SRAM: load={fields[2]:#x}, payload={fields[3]} B, '
          f'CRC32={fields[4]:08x}, flashed={len(result)} B')
