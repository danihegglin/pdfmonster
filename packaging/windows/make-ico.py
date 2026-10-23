"""Render packaging/pdfmonster.svg into a multi-size Windows .ico.

Usage: python make-ico.py <svg> <out.ico>   (needs `pip install resvg-py pillow`)
"""
import io
import sys

import resvg_py
from PIL import Image

svg, out = sys.argv[1], sys.argv[2]
png = bytes(resvg_py.svg_to_bytes(svg_path=svg, width=256, height=256))
Image.open(io.BytesIO(png)).save(
    out, sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
)
