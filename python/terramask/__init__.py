"""Masks, outlines, polygons and distance grids of the world's water, land,
forests, glaciers, parks or any layer of OpenStreetMap vector tiles, for any
area, from tiles fetched on demand; depth and height bands from terrain tiles.

    >>> import terramask
    >>> vineyard = (-70.85, 41.3, -70.45, 41.55)       # west, south, east, north
    >>> water = terramask.fetch(vineyard, width=1200)  # OpenFreeMap, cached
    >>> mask = water.mask(vineyard, 1200)
    >>> mask.coverage                                  # (h, w) float32, 1 = water
    >>> shore = mask.outlines()                        # [(n, 2) float32] in pixels
    >>> dist = mask.distance()                         # pixels to shore, + in water
    >>> water.geojson()                                # FeatureCollection in lon/lat

By default `fetch` gives the sea, lakes, rivers, canals and docks of the
OpenMapTiles schema; `select=` takes presets (`"land"`, `"forest"`, `"parks"`…, see `PRESETS`) and
`layer:class` rules instead. Depth and height bands come from terrain tiles,
fetched only by `fetch_elevation`:

    >>> sea = terramask.fetch(vineyard, width=1200, select="ocean")
    >>> terrain = terramask.fetch_elevation(vineyard, width=1200)
    >>> zones = sea.split(terrain, [-50, -20, -10])    # depths are negative

The work is done in Rust (the `terramask._terramask` extension); this module
adds numpy arrays.

Map data © OpenMapTiles © OpenStreetMap contributors. Terrain: see
https://github.com/tilezen/joerd/blob/master/docs/attribution.md
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Callable, Iterable, NamedTuple, Union

import numpy as np

from . import _terramask

__all__ = ["DEFAULT_AREAS", "DEFAULT_LINES", "MAX_ZOOM", "OPENFREEMAP", "PRESETS", "TERRARIUM", "TERRARIUM_MAX_ZOOM",
           "TERRARIUM_ZOOM", "AreaInfo", "Elevation", "Features", "Mask", "fetch", "fetch_elevation", "tiles_for",
           "zoom_for"]
__version__ = _terramask.__version__

Bounds = tuple[float, float, float, float]  # west, south, east, north (degrees)
Select = Union[str, Iterable[str]]  # presets and layer:class rules

OPENFREEMAP: str = _terramask.OPENFREEMAP
TERRARIUM: str = _terramask.TERRARIUM
TERRARIUM_ZOOM: int = _terramask.TERRARIUM_ZOOM
TERRARIUM_MAX_ZOOM: int = _terramask.TERRARIUM_MAX_ZOOM
MAX_ZOOM: int = _terramask.MAX_ZOOM
DEFAULT_AREAS: list[str] = list(_terramask.DEFAULT_AREAS)
DEFAULT_LINES: list[str] = list(_terramask.DEFAULT_LINES)
PRESETS: dict[str, str] = dict(_terramask.PRESETS)
"""Preset names and the rules they stand for."""


def _select(select) -> list[str] | None:
    if select is None:
        return None
    return [select] if isinstance(select, str) else list(select)


def _unpack(pts: bytes, ends: bytes) -> list[np.ndarray]:
    xy = np.frombuffer(pts, dtype="<f4").reshape(-1, 2)
    stops = np.frombuffer(ends, dtype="<u4")
    starts = np.concatenate([[0], stops[:-1]]).astype(int)
    return [xy[a:b] for a, b in zip(starts, stops.astype(int))]


class Mask:
    """Coverage of a north-up Web Mercator grid by the areas (water, unless
    something else was selected)."""

    def __init__(self, inner, bounds: Bounds):
        self._inner = inner
        self.bounds = bounds
        self.width: int = inner.width
        self.height: int = inner.height
        self.coverage: np.ndarray = np.frombuffer(inner.coverage(), dtype="<f4").reshape(self.height, self.width)
        """Fraction of each pixel covered, row 0 at the top."""

    def outlines(self) -> list[np.ndarray]:
        """The edge (the shoreline, for water) as (n, 2) arrays of x, y in
        pixels, the area on the left. Closed rings repeat their first point;
        lines leaving the grid are open."""
        return _unpack(*self._inner.outlines())

    def distance(self) -> np.ndarray:
        """Pixels to the edge: positive inside, negative outside, infinite
        when the grid has no edge."""
        return np.frombuffer(self._inner.distance(), dtype="<f4").reshape(self.height, self.width)

    @property
    def fraction(self) -> float:
        """Share of the grid that is covered."""
        return float(self.coverage.mean())

    def __repr__(self) -> str:
        return f"<terramask.Mask {self.width}×{self.height}, {self.fraction:.1%} covered>"


class AreaInfo(NamedTuple):
    layer: str
    cls: str
    low: float | None
    """Elevation band in metres, after `Features.split`; None at an open end or before."""
    high: float | None


class Features:
    """Areas and lines collected from vector tiles, cut to each tile."""

    def __init__(self, inner=None):
        self._inner = inner if inner is not None else _terramask.Features()

    def add_tile(self, z: int, x: int, y: int, data: bytes, *, select: Select | None = None,
                 areas: list[str] | None = None, lines: list[str] | None = None, intermittent: bool = False,
                 tunnels: bool = False) -> None:
        """Read one vector tile (raw or gzipped protobuf) from anywhere."""
        self._inner.add_tile(z, x, y, data, _select(select), areas, lines, intermittent, tunnels)

    def mask(self, bounds: Bounds, width: int, height: int | None = None, *,
             supersample: int = 4, line_width: float = 0.0) -> Mask:
        """Coverage on a `width` × `height` grid over `bounds` (height follows the
        box's shape when left out). `line_width` > 0 burns the lines in, in pixels."""
        return Mask(self._inner.mask(tuple(bounds), width, height, supersample, line_width), tuple(bounds))

    def lines(self, bounds: Bounds, width: int, height: int | None = None) -> list[tuple[str, np.ndarray]]:
        """Lines (waterway centre lines) as (class, (n, 2) pixels) on the grid."""
        pts, ends, classes = self._inner.lines(tuple(bounds), width, height)
        return list(zip(classes, _unpack(pts, ends)))

    def subset(self, select: Select) -> Features:
        """The areas and lines matching presets or `layer:class` rules."""
        return Features(self._inner.subset(_select(select)))

    def split(self, elevation: Elevation, levels: Iterable[float]) -> Features:
        """Every area cut into elevation bands at `levels` (metres, depths
        negative): below the lowest, between each pair, above the highest.
        Edges stay those of the areas; the terrain decides where bands meet.
        Areas beyond the elevation tiles are left out."""
        return Features(self._inner.split(elevation._inner, [float(v) for v in levels]))

    def within(self, low: float = -np.inf, high: float = np.inf) -> Features:
        """The bands of a split that lie within `low`..`high` metres."""
        return Features(self._inner.within(float(low), float(high)))

    @property
    def areas(self) -> list[AreaInfo]:
        """Layer, class and elevation band of each area."""
        return [AreaInfo(*a) for a in self._inner.area_info()]

    def geojson(self, *, pieces: bool = False, bounds: Bounds | None = None) -> dict:
        """A GeoJSON FeatureCollection in lon/lat, with `layer` and `class`
        properties (and `min`, `max` for bands): one MultiPolygon per layer,
        class and band, joined across tile edges, and the lines.
        `pieces=True` keeps the areas as the tiles cut them; `bounds` cuts
        everything to that box."""
        return json.loads(self._inner.geojson(pieces, None if bounds is None else tuple(bounds)))

    @property
    def area_count(self) -> int:
        return self._inner.area_count

    @property
    def line_count(self) -> int:
        return self._inner.line_count

    def __repr__(self) -> str:
        return f"<terramask.Features {self.area_count} areas, {self.line_count} lines>"


class Elevation:
    """Terrain heights in metres (sea floor negative) from elevation tiles."""

    def __init__(self, inner=None):
        self._inner = inner if inner is not None else _terramask.Elevation()

    def add_tile(self, z: int, x: int, y: int, data: bytes) -> None:
        """Read one Terrarium PNG from anywhere. Tiles must share one zoom."""
        self._inner.add_tile(z, x, y, data)

    def grid(self, bounds: Bounds, width: int, height: int | None = None) -> np.ndarray:
        """(h, w) float32 metres at the pixel centres; NaN outside the tiles."""
        data, h = self._inner.grid(tuple(bounds), width, height)
        return np.frombuffer(data, dtype="<f4").reshape(h, width)

    def at(self, lon: float, lat: float) -> float:
        return self._inner.at(lon, lat)

    @property
    def tile_count(self) -> int:
        return self._inner.tile_count

    def __repr__(self) -> str:
        return f"<terramask.Elevation {self.tile_count} tiles>"


def fetch(bounds: Bounds, width: int | None = None, height: int | None = None, *, zoom: int | None = None,
          select: Select | None = None, areas: list[str] | None = None, lines: list[str] | None = None,
          intermittent: bool = False, tunnels: bool = False, source: str | None = None,
          cache: str | Path | None = None, no_cache: bool = False, max_tiles: int = 256,
          log: Callable[[int, int], None] | None = None) -> Features:
    """Download (or read from the cache) the water in `bounds`, or what
    `select` names: presets (see `PRESETS`) and `layer:class` rules.

    `width` is the output width in pixels: it picks the tile zoom with at least
    that detail. Or give `zoom` directly. `source` is a TileJSON URL or a
    `{z}/{x}/{y}` template (default OpenFreeMap); `cache` defaults to
    `$TERRAMASK_CACHE` or the platform's cache folder. `log(done, total)` reports tiles.
    """
    inner = _terramask.fetch(tuple(bounds), width, height, zoom, _select(select), areas, lines, intermittent,
                             tunnels, source, None if cache is None else str(cache), no_cache, max_tiles, log)
    return Features(inner)


def fetch_elevation(bounds: Bounds, width: int | None = None, height: int | None = None, *,
                    zoom: int | None = None, max_zoom: int | None = None, source: str | None = None,
                    cache: str | Path | None = None, no_cache: bool = False, max_tiles: int = 256,
                    log: Callable[[int, int], None] | None = None) -> Elevation:
    """Download (or read from the cache) terrain for `bounds`: Terrarium tiles,
    by default from AWS Open Data (no key). Same arguments as `fetch`;
    `source` is a `{z}/{x}/{y}` template.

    The zoom picked from `width` stops at `max_zoom`, by default
    `TERRARIUM_ZOOM` (10): deeper, some coasts flatten the sea to 0 m, and
    the sea floor has no more detail anyway. For detailed land heights pass
    up to `TERRARIUM_MAX_ZOOM` (15)."""
    inner = _terramask.fetch_elevation(tuple(bounds), width, height, zoom, max_zoom, source,
                                       None if cache is None else str(cache), no_cache, max_tiles, log)
    return Elevation(inner)


def zoom_for(bounds: Bounds, width: int, max_tiles: int = 256, max_zoom: int = MAX_ZOOM) -> int:
    """Tile zoom with at least the detail of `width` pixels across `bounds`."""
    return _terramask.zoom_for(tuple(bounds), width, max_tiles, max_zoom)


def tiles_for(bounds: Bounds, zoom: int) -> list[tuple[int, int, int]]:
    """(z, x, y) of the tiles covering `bounds`; x may exceed 2**z - 1 across 180°."""
    return _terramask.tiles_for(tuple(bounds), zoom)
