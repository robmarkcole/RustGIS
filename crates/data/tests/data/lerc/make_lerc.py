"""Writes the LERC GeoTIFF test files next to this script, with GDAL (through rasterio).

    python -I make_lerc.py

Their values follow the formulas in `crates/data/src/raster/lerc.rs`'s tests, which compute them
again; every file is lossless (GDAL's default MAX_Z_ERROR of 0).
"""
import os

import numpy as np
import rasterio
from rasterio.transform import from_origin

HERE = os.path.dirname(os.path.abspath(__file__))


def grid(w, h):
    y, x = np.mgrid[0:h, 0:w]
    return x, y


def write(name, data, dtype, nodata=None, **opts):
    count, h, w = data.shape
    profile = dict(driver="GTiff", width=w, height=h, count=count, dtype=dtype, crs="EPSG:4326",
                   transform=from_origin(10.0, 46.0, 0.001, 0.001), nodata=nodata, **opts)
    with rasterio.open(os.path.join(HERE, name), "w", **profile) as dst:
        dst.write(data.astype(dtype))


# Float32, 100 × 70 in 32 × 32 tiles (edge tiles at the right and bottom), a hole of NaN (LERC's
# mask): x in 10..20, y in 5..9.
x, y = grid(100, 70)
f = ((x * 7 + y * 13) % 1000) / 8.0
f[5:9, 10:20] = np.nan
write("lerc_f32_tiles.tif", f[None], "float32", nodata=float("nan"), tiled=True, blockxsize=32, blockysize=32, compress="lerc")

# Three bands of UInt16, pixel-interleaved, 70 × 50 in 32 × 32 tiles, LERC wrapped in Deflate.
x, y = grid(70, 50)
u = np.stack([(x * 3 + y * 5 + b * 100) % 60000 for b in range(3)])
write("lerc_deflate_u16_3bands.tif", u, "uint16", tiled=True, blockxsize=32, blockysize=32, compress="lerc_deflate", interleave="pixel")

# Two bands of Int16, band-interleaved, in strips of 8 rows, 45 × 30, LERC wrapped in Zstandard.
x, y = grid(45, 30)
i = np.stack([x - y * 2 + b * 50 for b in range(2)])
write("lerc_zstd_i16_planar_strips.tif", i, "int16", tiled=False, blockysize=8, compress="lerc_zstd", interleave="band")

# Float64, 40 × 30 in 16 × 16 tiles, LERC wrapped in Zstandard.
x, y = grid(40, 30)
d = x * 0.25 - y * 1.5
write("lerc_zstd_f64.tif", d[None], "float64", tiled=True, blockxsize=16, blockysize=16, compress="lerc_zstd")

# Four bands of Byte, the last an unassociated alpha of only 0 and 255, pixel-interleaved, 40 × 30
# in 16 × 16 tiles: GDAL stores the alpha as LERC's mask, and the blob has three samples a pixel.
x, y = grid(40, 30)
rgba = np.stack([(x * 5 + y) % 256, (x + y * 7) % 256, (x * 3) % 256, np.where((x + y) % 3 == 0, 0, 255)])
write("lerc_u8_rgba_alpha_mask.tif", rgba, "uint8", tiled=True, blockxsize=16, blockysize=16, compress="lerc",
      interleave="pixel", photometric="rgb", alpha="yes")

# Three bands of Float32, pixel-interleaved, 40 × 30 in 16 × 16 tiles: NaN where (x + y) % 5 == 0
# in every band, and also where x == 7 in the first only. A pixel NaN in some bands only makes GDAL
# write one blob a band, each with its own mask; the last two masks are the same.
x, y = grid(40, 30)
v = np.stack([x * 1.5 + y + b * 100.0 for b in range(3)])
v[:, (x + y) % 5 == 0] = np.nan
v[0, x == 7] = np.nan
write("lerc_f32_band_masks.tif", v, "float32", nodata=float("nan"), tiled=True, blockxsize=16, blockysize=16, compress="lerc_zstd",
      interleave="pixel")
