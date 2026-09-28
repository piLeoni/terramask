"""Water masks for any area, from OpenStreetMap vector tiles fetched on demand.

    >>> import watermask
    >>> vineyard = (-70.85, 41.3, -70.45, 41.55)       # west, south, east, north
    >>> water = watermask.fetch(vineyard, width=1200)  # OpenFreeMap, cached
    >>> mask = water.mask(vineyard, 1200)
    >>> mask.coverage                                  # (h, w) float32, 1 = water
    >>> shore = mask.outlines()                        # [(n, 2) float32] in pixels
    >>> dist = mask.distance()                         # pixels to shore, + in water
    >>> water.geojson()                                # FeatureCollection in lon/lat

Water is the sea, lakes, rivers, canals and docks of the OpenMapTiles schema;
pass `areas=` / `lines=` to choose other classes. The work is done in Rust
(the `watermask._watermask` extension); this module adds numpy arrays.

Map data © OpenMapTiles © OpenStreetMap contributors.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Callable

import numpy as np

from . import _watermask

__all__ = ["DEFAULT_AREAS", "DEFAULT_LINES", "MAX_ZOOM", "OPENFREEMAP", "Mask", "Water",
           "fetch", "tiles_for", "zoom_for"]
__version__ = _watermask.__version__

Bounds = tuple[float, float, float, float]  # west, south, east, north (degrees)

OPENFREEMAP: str = _watermask.OPENFREEMAP
MAX_ZOOM: int = _watermask.MAX_ZOOM
DEFAULT_AREAS: list[str] = list(_watermask.DEFAULT_AREAS)
DEFAULT_LINES: list[str] = list(_watermask.DEFAULT_LINES)


def _unpack(pts: bytes, ends: bytes) -> list[np.ndarray]:
    xy = np.frombuffer(pts, dtype="<f4").reshape(-1, 2)
    stops = np.frombuffer(ends, dtype="<u4")
    starts = np.concatenate([[0], stops[:-1]]).astype(int)
    return [xy[a:b] for a, b in zip(starts, stops.astype(int))]


class Mask:
    """Water coverage of a north-up Web Mercator grid."""

    def __init__(self, inner, bounds: Bounds):
        self._inner = inner
        self.bounds = bounds
        self.width: int = inner.width
        self.height: int = inner.height
        self.coverage: np.ndarray = np.frombuffer(inner.coverage(), dtype="<f4").reshape(self.height, self.width)
        """Fraction of each pixel that is water, row 0 at the top."""

    def outlines(self) -> list[np.ndarray]:
        """The shoreline as (n, 2) arrays of x, y in pixels, water on the left.
        Closed rings repeat their first point; lines leaving the grid are open."""
        return _unpack(*self._inner.outlines())

    def distance(self) -> np.ndarray:
        """Pixels to the shore: positive in water, negative on land, infinite
        when the grid has no shore."""
        return np.frombuffer(self._inner.distance(), dtype="<f4").reshape(self.height, self.width)

    @property
    def water_fraction(self) -> float:
        return float(self.coverage.mean())

    def __repr__(self) -> str:
        return f"<watermask.Mask {self.width}×{self.height}, {self.water_fraction:.1%} water>"


class Water:
    """Water collected from vector tiles, cut to each tile."""

    def __init__(self, inner=None):
        self._inner = inner if inner is not None else _watermask.Water()

    def add_tile(self, z: int, x: int, y: int, data: bytes, *, areas: list[str] | None = None,
                 lines: list[str] | None = None, intermittent: bool = False, tunnels: bool = False) -> None:
        """Read one vector tile (raw or gzipped protobuf) from anywhere."""
        self._inner.add_tile(z, x, y, data, areas, lines, intermittent, tunnels)

    def mask(self, bounds: Bounds, width: int, height: int | None = None, *,
             supersample: int = 4, line_width: float = 0.0) -> Mask:
        """Coverage on a `width` × `height` grid over `bounds` (height follows the
        box's shape when left out). `line_width` > 0 burns waterway lines in, in pixels."""
        return Mask(self._inner.mask(tuple(bounds), width, height, supersample, line_width), tuple(bounds))

    def lines(self, bounds: Bounds, width: int, height: int | None = None) -> list[tuple[str, np.ndarray]]:
        """Waterway centre lines as (class, (n, 2) pixels) on the grid."""
        pts, ends, classes = self._inner.lines(tuple(bounds), width, height)
        return list(zip(classes, _unpack(pts, ends)))

    def geojson(self, *, pieces: bool = False, bounds: Bounds | None = None) -> dict:
        """A GeoJSON FeatureCollection in lon/lat: one MultiPolygon per area class,
        joined across tile edges, and the waterway lines. `pieces=True` keeps
        the areas as the tiles cut them; `bounds` cuts everything to that box."""
        return json.loads(self._inner.geojson(pieces, None if bounds is None else tuple(bounds)))

    @property
    def area_count(self) -> int:
        return self._inner.area_count

    @property
    def line_count(self) -> int:
        return self._inner.line_count

    def __repr__(self) -> str:
        return f"<watermask.Water {self.area_count} areas, {self.line_count} lines>"


def fetch(bounds: Bounds, width: int | None = None, height: int | None = None, *, zoom: int | None = None,
          areas: list[str] | None = None, lines: list[str] | None = None, intermittent: bool = False,
          tunnels: bool = False, source: str | None = None, cache: str | Path | None = None,
          no_cache: bool = False, max_tiles: int = 256,
          log: Callable[[int, int], None] | None = None) -> Water:
    """Download (or read from the cache) the water in `bounds`.

    `width` is the output width in pixels: it picks the tile zoom with at least
    that detail. Or give `zoom` directly. `source` is a TileJSON URL or a
    `{z}/{x}/{y}` template (default OpenFreeMap); `cache` defaults to
    `$WATERMASK_CACHE` or the platform's cache folder. `log(done, total)` reports tiles.
    """
    inner = _watermask.fetch(tuple(bounds), width, height, zoom, areas, lines, intermittent, tunnels, source,
                             None if cache is None else str(cache), no_cache, max_tiles, log)
    return Water(inner)


def zoom_for(bounds: Bounds, width: int, max_tiles: int = 256, max_zoom: int = MAX_ZOOM) -> int:
    """Tile zoom with at least the detail of `width` pixels across `bounds`."""
    return _watermask.zoom_for(tuple(bounds), width, max_tiles, max_zoom)


def tiles_for(bounds: Bounds, zoom: int) -> list[tuple[int, int, int]]:
    """(z, x, y) of the tiles covering `bounds`; x may exceed 2**z - 1 across 180°."""
    return _watermask.tiles_for(tuple(bounds), zoom)
