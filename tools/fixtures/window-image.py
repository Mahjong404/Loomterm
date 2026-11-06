"""Deterministic 32x32 synthetic PNG/Kitty input; no external image assets."""
import argparse
import base64
import hashlib
import os
from pathlib import Path
import struct
import sys
import zlib


def png():
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    rows = b"".join(b"\0" + b"".join(bytes((240, 16, 240) if (x // 8 + y // 8) % 2 else (16, 240, 16))
                                   for x in range(32)) for y in range(32))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 32, 32, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b""))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--delete", action="store_true")
    args = parser.parse_args()
    if os.name == "nt" and not args.output:
        import ctypes
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.GetStdHandle.restype = ctypes.c_void_p
        handle = kernel.GetStdHandle(-11)
        mode = ctypes.c_ulong()
        if kernel.GetConsoleMode(ctypes.c_void_p(handle), ctypes.byref(mode)):
            assert kernel.SetConsoleMode(ctypes.c_void_p(handle), mode.value | 4)
    if args.output:
        args.output.write_bytes(png())
        print(hashlib.sha256(png()).hexdigest())
    elif args.delete:
        sys.stdout.buffer.write(b"\x1b_Ga=d,d=I,i=4242,q=2\x1b\\")
    else:
        sys.stdout.buffer.write(b"\x1b[2J\x1b[HM3_PNG_START\r\n\x1b_Ga=T,f=100,i=4242,q=2,c=8,r=4;"
                                + base64.b64encode(png()) + b"\x1b\\\r\n")
    sys.stdout.flush()
