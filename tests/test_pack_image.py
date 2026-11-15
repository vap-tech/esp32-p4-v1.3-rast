"""python3 -m unittest discover -s tests -p 'test_pack_image.py'."""
import gzip
import importlib.util
from pathlib import Path
import struct
import unittest
import zlib

spec = importlib.util.spec_from_file_location('pack_image', Path(__file__).parents[1] / 'scripts/pack-image.py')
packer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packer)


class PackImage(unittest.TestCase):
    def raw(self, upper=0x4ff40000, entry=0x30100000):
        header = bytearray(24)
        header[0] = 0xe9
        header[23] = 1
        struct.pack_into('<I', header, 4, entry)
        return packer.make_rom(header, [(0x30100000, b'BOOT' * 8 + packer.BINDING), (upper, b'RUNTIME!' * 16)])

    def test_split_preserves_rom_contract_and_crc_payload(self):
        raw = self.raw()
        header, segments = packer.parse_rom(raw)
        self.assertEqual(packer.make_rom(header, segments), raw)
        packed = packer.pack(raw)
        rom = packer.make_rom(header, segments[:1])
        spm = packer.parse_rom(packed[:len(rom)])[1][0]
        self.assertEqual(spm[0], segments[0][0])
        self.assertEqual(spm[1][:40], segments[0][1][:40])
        crc, inverted = struct.unpack_from('<II', spm[1], 40)
        self.assertEqual(crc, zlib.crc32(segments[1][1]))
        self.assertEqual(crc ^ inverted, 0xffffffff)
        offset = packer.MANIFEST - packer.FLASH_BASE
        fields = struct.unpack_from('<8s6I32s', packed, offset)
        self.assertEqual(fields[:4], (b'P4UPPER1', 1, 0x4ff40000, 128))
        self.assertEqual(fields[4], zlib.crc32(segments[1][1]))
        self.assertEqual(fields[5:], (packer.PAYLOAD, 0, bytes(32)))
        self.assertEqual(packed[offset + 64:], segments[1][1])
        self.assertEqual(packed[len(rom):offset], b'\xff' * (offset-len(rom)))

    def test_flash_asset_is_separate_bounded_and_checksummed(self):
        html = b'<html>Flashbox</html>' * 100
        packed = packer.pack(self.raw(), html)
        at = packer.ASSET - packer.FLASH_BASE
        magic, size, crc = struct.unpack_from('<8sII', packed, at)
        self.assertEqual(magic, b'P4WEB001')
        asset = packed[at + 16:at + 16 + size]
        self.assertEqual(zlib.crc32(asset), crc)
        self.assertEqual(gzip.decompress(asset), html)
        self.assertEqual(len(packed) % 4, 0)
        self.assertEqual(packer.pack(self.raw(), html), packed)
        # Reject asset overflow rather than truncating it.
        import random
        with self.assertRaises(ValueError):
            packer.pack(self.raw(), random.Random(123).randbytes(70000))

    def test_rejects_rom_corruption_truncation_and_trailing_bytes(self):
        raw = self.raw()
        for candidate in [raw[:20], raw[:-1], raw + b'\0', raw[:32] + bytes([raw[32] ^ 1]) + raw[33:]]:
            with self.assertRaises(ValueError):
                packer.pack(candidate)

    def test_rejects_unknown_regions_stack_and_nonresident_entry(self):
        for address in [0x4ff28000, 0x88000000, 0x4ffac000, 0x4ffabfc0, 0x4ffbc000, 0x4ffbbfc0]:
            with self.assertRaises(ValueError):
                packer.pack(self.raw(address))
        with self.assertRaises(ValueError):
            packer.pack(self.raw(entry=0x4ff40000))

    def test_rejects_missing_or_duplicate_binding_slot(self):
        header, segments = packer.parse_rom(self.raw())
        for spm in [b'BOOT' * 8, segments[0][1] + packer.BINDING]:
            with self.assertRaises(ValueError):
                packer.pack(packer.make_rom(header, [(segments[0][0], spm), segments[1]]))


if __name__ == '__main__':
    unittest.main()
