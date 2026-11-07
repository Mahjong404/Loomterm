"""Validate the synthetic PNG contract independently of a terminal renderer."""
import importlib.util
from pathlib import Path
import struct
import subprocess
import sys
import unittest
import zlib

source = Path(__file__).with_name("window-image.py")
spec = importlib.util.spec_from_file_location("window_image", source)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class SyntheticImageTests(unittest.TestCase):
    def test_valid_png_dimensions_crc_and_pixel_colors(self):
        data = fixture.png()
        self.assertEqual(data[:8], b"\x89PNG\r\n\x1a\n")
        offset, pixels = 8, b""
        while offset < len(data):
            length = struct.unpack(">I", data[offset:offset + 4])[0]
            kind = data[offset + 4:offset + 8]
            body = data[offset + 8:offset + 8 + length]
            crc = struct.unpack(">I", data[offset + 8 + length:offset + 12 + length])[0]
            self.assertEqual(crc, zlib.crc32(kind + body))
            if kind == b"IHDR":
                self.assertEqual(struct.unpack(">IIBBBBB", body), (32, 32, 8, 2, 0, 0, 0))
            if kind == b"IDAT":
                pixels += zlib.decompress(body)
            offset += 12 + length
        self.assertEqual(len(pixels), 32 * (1 + 32 * 3))
        self.assertEqual(pixels[1:4], bytes((16, 240, 16)))
        self.assertEqual(pixels[1 + 8 * 3:4 + 8 * 3], bytes((240, 16, 240)))

    def test_delete_is_control_only(self):
        result = subprocess.run([sys.executable, str(source), "--delete"],
                                capture_output=True, check=True, timeout=5)
        self.assertEqual(result.stdout, b"\x1b_Ga=d,d=I,i=4242,q=2\x1b\\")

    def test_replacement_keeps_image_identity_but_changes_pixels(self):
        original = fixture.png()
        alternate = fixture.png(alternate=True)
        self.assertNotEqual(original, alternate)
        output = subprocess.run([sys.executable, str(source), "--replace"],
                                capture_output=True, check=True, timeout=5).stdout
        self.assertTrue(output.startswith(b"\x1b[H\x1b_Ga=T,f=100,i=4242,q=2,c=8,r=4;"))
        self.assertIn(b"M3_PNG_REPLACE", output)
        self.assertIn(__import__("base64").b64encode(alternate), output)

    def test_invalid_replacement_is_bounded_truncated_png(self):
        output = subprocess.run([sys.executable, str(source), "--invalid"],
                                capture_output=True, check=True, timeout=5).stdout
        self.assertTrue(output.startswith(b"\x1b[H\x1b_Ga=T,f=100,i=4242,q=2,c=8,r=4;"))
        self.assertIn(__import__("base64").b64encode(fixture.png()[:len(fixture.png()) // 2]), output)
        self.assertLess(len(output), 1024)

    def test_overbudget_png_is_compact_but_decodes_above_64_mib(self):
        data = fixture.overbudget_png()
        self.assertEqual(struct.unpack(">II", data[16:24]), (4097, 4096))
        self.assertGreater(4097 * 4096 * 4, 64 * 1024 * 1024)
        self.assertLess(len(data), 200_000)


if __name__ == "__main__":
    unittest.main()
