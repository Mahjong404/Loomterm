"""Deterministic UTF-8/cell-layout fixture, emitted by the work process itself."""
import argparse
import ctypes
import os
import sys
import time


def payload():
    return ("\x1b[2J\x1b[HASCII |abcd| END\r\n"
            "CJK   |中文| END\r\nCOMB  |e\u0301 e| END\r\n"
            "EMOJI |\U0001f600\U0001f680| END\r\n"
            "ZWJ   |\U0001f469\u200d\U0001f4bb| END\r\n"
            "VS16  |\u2764\ufe0f| END\r\nM3_TEXT_READY\r\n").encode("utf-8")


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--hold", type=int, choices=range(0, 61), default=0)
    args = p.parse_args()
    if os.name == "nt":
        k = ctypes.WinDLL("kernel32", use_last_error=True)
        k.GetStdHandle.restype = ctypes.c_void_p
        handle = k.GetStdHandle(-11)
        mode = ctypes.c_ulong()
        if k.GetConsoleMode(ctypes.c_void_p(handle), ctypes.byref(mode)):
            assert k.SetConsoleMode(ctypes.c_void_p(handle), mode.value | 4)
    sys.stdout.buffer.write(payload())
    sys.stdout.flush()
    time.sleep(args.hold)
