"""Offline tests on real OpenFreeMap tiles around Martha's Vineyard."""

from pathlib import Path

import numpy as np
import pytest

import watermask

FIXTURES = Path(__file__).parents[2] / "crates" / "watermask" / "tests" / "fixtures"


def tile_bounds(z, x, y):
    import math
    n = 2 ** z

    def lat(row):
        return math.degrees(math.atan(math.sinh(math.pi * (1 - 2 * row / n))))

    return (x / n * 360 - 180, lat(y + 1), (x + 1) / n * 360 - 180, lat(y))


def load(*tiles, **filter):
    w = watermask.Water()
    for z, x, y in tiles:
        w.add_tile(z, x, y, (FIXTURES / f"{z}-{x}-{y}.pbf").read_bytes(), **filter)
    return w


def test_open_sea_is_water():
    m = load((12, 1244, 1531)).mask(tile_bounds(12, 1244, 1531), 256, 256)
    assert m.coverage.shape == (256, 256)
    assert m.coverage.dtype == np.float32
    assert m.water_fraction > 0.999


def test_outlines_and_distance():
    b = tile_bounds(12, 1244, 1528)
    m = load((12, 1244, 1528)).mask(b, 256, 256)
    shore = m.outlines()
    assert shore and all(s.ndim == 2 and s.shape[1] == 2 for s in shore)
    d = m.distance()
    assert d.shape == (256, 256)
    assert (d[m.coverage >= 0.5] > 0).all() and (d[m.coverage < 0.5] < 0).all()


def test_filter_and_lines():
    b = tile_bounds(12, 1244, 1529)
    all_water = load((12, 1244, 1529)).mask(b, 256, 256).water_fraction
    sea = load((12, 1244, 1529), areas=["ocean"]).mask(b, 256, 256).water_fraction
    assert sea < all_water
    w = load((12, 1244, 1529))
    assert all(cls in watermask.DEFAULT_LINES for cls, _ in w.lines(b, 256, 256))


def test_geojson():
    w = load((12, 1244, 1529))
    g = w.geojson(pieces=True)
    assert g["type"] == "FeatureCollection"
    assert len(g["features"]) == w.area_count + w.line_count
    lon, lat = g["features"][0]["geometry"]["coordinates"][0][0][0]
    assert -70.7 < lon < -70.5 and 41.3 < lat < 41.5


def test_geojson_joins_pieces_across_tiles():
    w = load((12, 1243, 1528), (12, 1244, 1528))
    areas = [f for f in w.geojson()["features"] if f["geometry"]["type"] == "MultiPolygon"]
    classes = [f["properties"]["class"] for f in areas]
    assert sorted(classes) == sorted(set(classes)) and len(areas) < w.area_count


def test_geojson_bounds():
    b = tile_bounds(12, 1244, 1529)
    box = (b[0], b[1], (b[0] + b[2]) / 2, b[3])
    for pieces in (False, True):
        g = load((12, 1244, 1529)).geojson(pieces=pieces, bounds=box)

        def points(c):
            return [c] if isinstance(c[0], float) else [p for x in c for p in points(x)]

        lons = [p[0] for f in g["features"] for p in points(f["geometry"]["coordinates"])]
        assert lons and box[0] - 1e-6 <= min(lons) and max(lons) <= box[2] + 1e-6


def test_height_follows_the_box():
    m = load((12, 1244, 1531)).mask((-70.7, 41.0, -70.5, 41.2), 400)
    assert m.width == 400 and 490 < m.height < 540


def test_tiles_and_zoom():
    b = (-70.85, 41.3, -70.45, 41.55)
    assert watermask.zoom_for(b, 1200) == 13
    assert watermask.tiles_for((4.9, 52.37, 4.9001, 52.3701), 12) == [(12, 2103, 1346)]


def test_bad_arguments():
    with pytest.raises(ValueError):
        watermask.fetch((0, 0, 1, 1))
    with pytest.raises(RuntimeError):
        watermask.Water().add_tile(12, 1, 1, b"\x1a\xff\xff\xff")
    with pytest.raises(ValueError, match="forest"):
        load((12, 1244, 1529), select="forrest")
    with pytest.raises(ValueError, match="not both"):
        load((12, 1244, 1529), select="ocean", areas=["lake"])


def test_presets_and_land():
    b = tile_bounds(12, 1244, 1529)
    assert {"water", "land", "forest", "glacier", "parks"} <= watermask.PRESETS.keys()
    w = load((12, 1244, 1529), select=["forest", "parks", "land"])
    layers = {a.layer for a in w.areas}
    assert {"landcover", "land"} <= layers <= {"landcover", "park", "land"}
    woods = w.subset("forest")
    assert woods.area_count and all(a.layer == "landcover" and a.cls == "wood" for a in woods.areas)
    props = [f["properties"] for f in w.geojson()["features"]]
    assert {"layer": "landcover", "class": "wood"} in props
    sea = load((12, 1244, 1529), select="ocean").mask(b, 256, 256).coverage
    land = w.subset("land").mask(b, 256, 256).coverage
    assert abs((sea + land).mean() - 1) < 1e-3


def elevation(*tiles):
    e = watermask.Elevation()
    for z, x, y in tiles:
        e.add_tile(z, x, y, (FIXTURES / f"dem-{z}-{x}-{y}.png").read_bytes())
    return e


def test_elevation_and_depth_bands():
    e = elevation((12, 1244, 1531))
    b = tile_bounds(12, 1244, 1531)
    h = e.grid(b, 128)
    assert h.shape == (128, 128) and h.dtype == np.float32
    assert (h < 5).all() and h.min() < -20
    assert np.isnan(e.at(0, 0))

    sea = load((12, 1244, 1531), select="ocean")
    zones = sea.split(e, [-10, -20])
    bands = {(a.low, a.high) for a in zones.areas}
    assert {(None, -20.0), (-20.0, -10.0)} <= bands
    deep = zones.within(high=-20).mask(b, 128).coverage
    assert (h[deep > 0.99] <= -19).mean() > 0.98
    whole = sea.mask(b, 128).coverage
    assert abs(zones.mask(b, 128).coverage - whole).mean() < 1e-3
    feats = zones.geojson()["features"]
    assert any(f["properties"].get("max") == -20 and f["properties"]["min"] is None for f in feats)
    with pytest.raises(ValueError):
        sea.split(watermask.Elevation(), [0])
