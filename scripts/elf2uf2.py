#!/usr/bin/env python3
"""Convert a firmware ELF to UF2 for BOOTSEL drag-and-drop.

elf2uf2-rs only knows the RP2040, so this handles the RP2350 (Pico 2 W) too.
For the RP2040 the output is byte-identical to elf2uf2-rs (CI checks it).

    elf2uf2.py [--family rp2350|rp2040] [--deploy] firmware.elf [out.uf2]

--deploy copies the result onto a Pico mounted in BOOTSEL mode (the drive
with an INFO_UF2.TXT), which is what `cargo run` uses. No dependencies
beyond the standard library.
"""

import argparse
import glob
import os
import shutil
import struct
import sys
import tempfile

MAGIC_START0 = 0x0A324655
MAGIC_START1 = 0x9E5D5157
MAGIC_END = 0x0AB16F30
FLAG_FAMILY_ID_PRESENT = 0x00002000
FLAG_EXTENSION_FLAGS_PRESENT = 0x00008000
EXTENSION_RP2_IGNORE_BLOCK = 0x9957E304
PAGE = 256

FAMILIES = {
    "rp2040": 0xE48BFF56,
    "rp2350": 0xE48BFF59,  # RP2350, Arm, secure
}
ABSOLUTE_FAMILY = 0xE48BFF57

FLASH_START = 0x10000000
FLASH_END = 0x11000000
# picotool's workaround for RP2350-E10: a block targeting the end of flash,
# in the "absolute" family, marked to be ignored, ahead of the image.
ABS_BLOCK_ADDR = FLASH_END - PAGE

PT_LOAD = 1


def load_segments(elf):
    """(physical address, bytes) of each loaded ELF segment with file data."""
    if elf[:4] != b"\x7fELF" or elf[4] != 1 or elf[5] != 1:
        sys.exit("elf2uf2: not a little-endian 32-bit ELF")
    phoff, = struct.unpack_from("<I", elf, 28)
    phentsize, phnum = struct.unpack_from("<HH", elf, 42)
    segments = []
    for i in range(phnum):
        p_type, p_offset, _vaddr, p_paddr, p_filesz, _memsz, _flags, _align = struct.unpack_from(
            "<8I", elf, phoff + i * phentsize
        )
        if p_type != PT_LOAD or p_filesz == 0:
            continue
        if not (FLASH_START <= p_paddr and p_paddr + p_filesz <= FLASH_END):
            sys.exit(f"elf2uf2: segment at {p_paddr:#010x} is outside flash; only flash images are supported")
        segments.append((p_paddr, elf[p_offset : p_offset + p_filesz]))
    if not segments:
        sys.exit("elf2uf2: no loadable segments")
    return segments


def pages_of(segments):
    """256-byte flash pages with the segments' bytes, zero-filled elsewhere."""
    pages = {}
    for addr, data in segments:
        for i, byte in enumerate(data):
            a = addr + i
            page = pages.setdefault(a - a % PAGE, bytearray(PAGE))
            page[a % PAGE] = byte
    return dict(sorted(pages.items()))


def block(flags, target, payload, block_no, num_blocks, family, extension=None):
    data = bytearray(476)
    data[: len(payload)] = payload
    if extension is not None:
        struct.pack_into("<I", data, PAGE, extension)
    return (
        struct.pack("<8I", MAGIC_START0, MAGIC_START1, flags, target, PAGE, block_no, num_blocks, family)
        + bytes(data)
        + struct.pack("<I", MAGIC_END)
    )


def convert(elf, family, also=(), blank=()):
    pages = pages_of(load_segments(elf))
    for other in also:
        pages.update(pages_of(load_segments(other)))
    for start, length in blank:
        for addr in range(start, start + length, PAGE):
            pages[addr] = b"\xff" * PAGE
    pages = dict(sorted(pages.items()))
    out = bytearray()
    if family == "rp2350":
        out += block(
            FLAG_FAMILY_ID_PRESENT | FLAG_EXTENSION_FLAGS_PRESENT,
            ABS_BLOCK_ADDR,
            b"\xef" * PAGE,
            0,
            2,
            ABSOLUTE_FAMILY,
            EXTENSION_RP2_IGNORE_BLOCK,
        )
    for n, (addr, data) in enumerate(pages.items()):
        out += block(FLAG_FAMILY_ID_PRESENT, addr, data, n, len(pages), FAMILIES[family])
    return bytes(out)


def find_bootsel_drive():
    roots = ["/media/*/*", "/run/media/*/*", "/media/*", "/mnt/*", "/Volumes/*"]
    for pattern in roots:
        for path in glob.glob(pattern):
            if os.path.isfile(os.path.join(path, "INFO_UF2.TXT")):
                return path
    return None


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--family", choices=sorted(FAMILIES), default="rp2350")
    ap.add_argument("--deploy", action="store_true", help="copy to a Pico in BOOTSEL mode")
    ap.add_argument("--also", action="append", default=[], metavar="ELF", help="add another image, e.g. the bootloader")
    ap.add_argument("--blank", action="append", default=[], metavar="ADDR:LEN", help="fill this flash range with 0xFF")
    ap.add_argument("elf")
    ap.add_argument("uf2", nargs="?")
    args = ap.parse_args()

    also = []
    for path in args.also:
        with open(path, "rb") as f:
            also.append(f.read())
    blank = [tuple(int(part, 0) for part in spec.split(":")) for spec in args.blank]
    with open(args.elf, "rb") as f:
        uf2 = convert(f.read(), args.family, also, blank)

    out = args.uf2
    if out is None:
        if not args.deploy:
            out = os.path.splitext(args.elf)[0] + ".uf2"
        else:
            out = os.path.join(tempfile.mkdtemp(), "firmware.uf2")
    with open(out, "wb") as f:
        f.write(uf2)
    print(f"wrote {out} ({len(uf2) // 512} blocks, {args.family})")

    if args.deploy:
        drive = find_bootsel_drive()
        if drive is None:
            sys.exit("elf2uf2: no Pico in BOOTSEL mode found (hold BOOTSEL while plugging it in)")
        shutil.copy(out, os.path.join(drive, "firmware.uf2"))
        print(f"copied to {drive}")


if __name__ == "__main__":
    main()
