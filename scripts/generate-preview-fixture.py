#!/usr/bin/env python3
"""Generate the byte-pinned 4000x3000 gradient JPEG for the preview corpus."""

import argparse
from pathlib import Path
import struct
import sys


def generate_gradient_jpeg():
    """Return the fixed gradient-jpeg-v1 baseline JPEG as bytes."""
    width, height = 4000, 3000
    jpeg = bytearray(b'\xff\xd8')  # SOI

    def segment(marker, payload):
        jpeg.extend(struct.pack('>BBH', 0xff, marker, len(payload) + 2))
        jpeg.extend(payload)

    segment(0xe0, b'JFIF\0\x01\x01\x00\x00\x01\x00\x01\x00\x00')
    # Quantization tables 0 (Y) and 1 (Cb and Cr), both constant 16.
    quantizers = bytes([16]) * 64
    segment(0xdb, b'\x00' + quantizers + b'\x01' + quantizers)
    # SOF0: 8-bit samples, 4:4:4 (one block per component per MCU).
    segment(0xc0, struct.pack('>BHHB', 8, height, width, 3)
            + bytes([1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]))
    # Identical luminance (0) and chrominance (1) Huffman tables:
    # DC codes 0000 through 1011 for categories 0..11; AC code 0 for EOB.
    for table in (0, 1):
        segment(0xc4, bytes([table, 0, 0, 0, 12]) + bytes(12) + bytes(range(12)))
        segment(0xc4, bytes([0x10 | table, 1]) + bytes(16))
    # SOS: Y uses table 0 and Cb and Cr use table 1, in one baseline scan.
    segment(0xda, bytes([3, 1, 0, 2, 0x11, 3, 0x11, 0, 63, 0]))

    pending = nbits = 0

    def write_bits(value, count):
        nonlocal pending, nbits
        pending = (pending << count) | value
        nbits += count
        while nbits >= 8:
            nbits -= 8
            byte = pending >> nbits
            jpeg.append(byte)
            if byte == 0xff:
                jpeg.append(0)  # Stuff entropy bytes that look like markers.
            pending &= (1 << nbits) - 1

    previous = [0, 0, 0]
    for gy in range(height // 8):
        for gx in range(width // 8):
            # For a flat block, quantized DC = 8 * (sample - 128) / 16.
            dc_values = ((gx + gy) % 64 - 32, gx % 16 - 8, gy % 16 - 8)
            for component, dc in enumerate(dc_values):
                diff = dc - previous[component]
                previous[component] = dc
                category = abs(diff).bit_length()
                write_bits(category, 4)
                if category:
                    # JPEG represents negative amplitudes by complementing the magnitude.
                    amplitude = diff if diff > 0 else diff + (1 << category) - 1
                    write_bits(amplitude, category)
                write_bits(0, 1)  # EOB

    if nbits:
        padding = 8 - nbits
        write_bits((1 << padding) - 1, padding)
    jpeg.extend(b'\xff\xd9')  # EOI
    return bytes(jpeg)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, help='output file (default: stdout)')
    args = parser.parse_args()
    jpeg = generate_gradient_jpeg()
    if args.output is None:
        sys.stdout.buffer.write(jpeg)
    else:
        args.output.write_bytes(jpeg)


if __name__ == '__main__':
    main()
