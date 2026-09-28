# watermask

Water masks for any area on Earth: the sea, lakes, rivers, canals and docks as
a coverage grid, shoreline polylines and a distance-to-shore field. The water
comes from OpenStreetMap vector tiles fetched on demand, so there is nothing to
download in advance: only the tiles covering the area, at the zoom that matches
the output resolution.

One Rust core, published for Rust, Python, Node.js and the browser (WebAssembly).

![Cape Cod, Martha's Vineyard and Nantucket: the shoreline, and lines following it out to sea](https://raw.githubusercontent.com/piLeoni/watermask/v0.1.0/docs/cape-cod.png)

<sub>Cape Cod and the islands. The shoreline is `mask.outlines()`; the lines at sea are
outlines of `mask.distance()` at growing distances, fading out.</sub>

<p>
<img src="https://raw.githubusercontent.com/piLeoni/watermask/v0.1.0/docs/amsterdam.png" width="32%" alt="Amsterdam: the canal ring and the IJ">
<img src="https://raw.githubusercontent.com/piLeoni/watermask/v0.1.0/docs/venice.png" width="32%" alt="Venice and its lagoon">
<img src="https://raw.githubusercontent.com/piLeoni/watermask/v0.1.0/docs/stockholm.png" width="32%" alt="Stockholm: islands between lake Mälaren and the Baltic">
</p>

<sub>Amsterdam, Venice, Stockholm: `mask.coverage` in grey, the shoreline in black,
waterway centre lines in grey. `cargo run --release --example readme` redraws
every image here from live tiles.</sub>

```python
import watermask

vineyard = (-70.85, 41.3, -70.45, 41.55)          # west, south, east, north
water = watermask.fetch(vineyard, width=1200)     # tiles are cached after the first run
mask = water.mask(vineyard, 1200)

mask.coverage      # (1000, 1200) float32, fraction of each pixel that is water
mask.outlines()    # shoreline: [(n, 2) float32] in pixels, water on the left
mask.distance()    # pixels to the shore, positive in water, negative on land
water.geojson()    # the water as GeoJSON in lon/lat, one MultiPolygon per class
```

## Why

Deriving water from elevation (everything at or below 0 m) misses lakes and
rivers, which sit above sea level, and floods land that lies below it: the
Netherlands, deltas, reclaimed coasts. Complete coastline and water datasets
exist (the OSM water polygons), but they are gigabytes and have to be tiled
and stored before use. Vector tile services already serve that data cut into
tiles, for free, one small tile at a time.

## Where the water comes from

By default [OpenFreeMap](https://openfreemap.org): the whole planet in the
[OpenMapTiles schema](https://openmaptiles.org/schema/), rebuilt weekly from
OpenStreetMap, free with no key and no request limits. Any source in that
schema works (MapTiler, a self-hosted copy of OpenFreeMap's planet file): pass
its TileJSON URL or a `{z}/{x}/{y}` URL template.

Two layers are read:

| layer      | kind     | classes (default in **bold**)                                         |
|------------|----------|-----------------------------------------------------------------------|
| `water`    | areas    | **ocean**, **lake**, **river**, **dock**, pond, swimming_pool        |
| `waterway` | lines    | **river**, **canal**, **stream**, ditch, drain                        |

`river` areas are the water surface of wide rivers and canals; `river` lines
are centre lines, of every river, however narrow. Seasonal water
(`intermittent`) and water underground (`tunnels`) are left out unless asked
for.

At zoom 14, the deepest, a tile is about 2.4 km across at the equator and its
coordinates are quantised to about 0.6 m; OpenMapTiles simplifies geometry
there by less than that, so this is full OpenStreetMap detail. At lower zooms
tiles are simplified and small features dropped, in step with the pixel size.

**Attribution.** Maps made with this data must credit
"© OpenMapTiles © OpenStreetMap contributors".

## How it works

1. **Zoom.** The shallowest zoom whose tiles (drawn 256 px wide) have pixels
   no larger than the output's, capped at the source's deepest zoom, then
   lowered while the area needs more than `max_tiles` tiles (default 256).
2. **Tiles.** Fetched in parallel and cached on disk, only their water layers
   (a fifth of the bytes or less). The cache is `$WATERMASK_CACHE`, or else
   `~/Library/Caches/watermask` on macOS, `%LOCALAPPDATA%\watermask` on
   Windows, `~/.cache/watermask` on Linux. A cached tile is used for 30 days,
   across OpenFreeMap's weekly builds, then downloaded again; when offline,
   older tiles are used anyway. Tiles past 30 days that nobody asks for are
   deleted, checked once a day. The TileJSON is read each run, since
   OpenFreeMap's tile URLs change with every build, and the last good copy is
   used when offline.
3. **Decoding.** A small protobuf reader pulls the two layers out of each tile
   and cuts every feature to its own tile, so the buffer tiles share with
   their neighbours is not counted twice.
4. **Mask.** Polygons are filled with the nonzero winding rule, so the many
   pieces of one sea join without seams and islands stay holes. Each pixel
   gets its exact covered fraction along the row and four sub-rows down it.
5. **Shoreline.** Marching squares at coverage ½, joined into polylines with
   water on the left.
6. **Distance.** An exact Euclidean distance transform, signed.
7. **Polygons.** For GeoJSON, the pieces of each class are joined with a
   polygon union ([i_overlay](https://github.com/iShape-Rust/iOverlay)),
   same nonzero rule as the mask, so the polygons cover what the mask covers.
   The output is OGC-valid (shapely and GEOS take it as is), exteriors
   counter-clockwise and holes clockwise, and can be cut to a box.

Masks are on a north-up Web Mercator grid. In Rust, `Water::mask_with` takes
any projection instead.

## Rust

```toml
[dependencies]
watermask = "0.2"          # default-features = false drops the HTTP client
```

```rust
use watermask::{Fetcher, Filter, GeoJsonOptions, Grid, MaskOptions, ZoomLimits};

let bounds = [-70.85, 41.3, -70.45, 41.55];
let grid = Grid::with_width(bounds, 1200);
let water = Fetcher::new().water_for(&grid, &Filter::default(), &ZoomLimits::default(), |done, total| {
    eprintln!("{done}/{total} tiles");
})?;
let mask = water.mask(&grid, &MaskOptions::default());
let shore = mask.outlines();
let dist = mask.distance();

// Any projection: lon, lat → pixel.
let tm = water.mask_with(800, 600, &MaskOptions::default(), |lon, lat| my_projection(lon, lat));

// Polygons: joined per class and cut to the box (Water::merged gives the same as structs).
let json = water.to_geojson(&GeoJsonOptions { bounds: Some(bounds), ..Default::default() });
```

Without the `fetch` feature, bring tiles from anywhere:

```rust
let mut water = watermask::Water::new();
for id in watermask::tiles_for(&bounds, 13) {
    water.add_tile(id, &bytes_of(id), &Filter::default())?;
}
```

`cargo run --release --example render -- -70.85,41.3,-70.45,41.55 1200 out.png`
draws the mask, shoreline and waterways of an area.

## Python

```sh
pip install watermask
```

The example at the top covers most uses. Also:
`watermask.fetch(bounds, width, areas=["ocean"], lines=[], source=..., log=print)`,
`water.lines(bounds, width)` for waterway centre lines as `(class, (n, 2) array)`,
`water.mask(..., line_width=1.5)` to burn them into the mask,
`Water().add_tile(z, x, y, data)` for your own tiles,
`zoom_for(bounds, width)` and `tiles_for(bounds, zoom)`.

`water.geojson(bounds=vineyard)` cuts the polygons and lines to the box;
`water.geojson(pieces=True)` keeps the areas as the tiles cut them. Into
shapely:

```python
from shapely.geometry import shape

sea = next(shape(f["geometry"]) for f in water.geojson(bounds=vineyard)["features"]
           if f["properties"]["class"] == "ocean")
```

## Node.js

```sh
npm install @pileoni/watermask
```

```js
const wm = require('@pileoni/watermask')

const vineyard = [-70.85, 41.3, -70.45, 41.55]
const water = wm.fetch(vineyard, { width: 1200 })   // blocks until the tiles are in
const mask = water.mask(vineyard, 1200)
mask.coverage()                 // Float32Array, row 0 at the top
const [pts, ends] = mask.outlines()   // x,y pairs; ends[i] = end of line i, in points
mask.distance()                 // Float32Array
water.geojson({ bounds: vineyard })   // string; also { pieces: true }
```

## Browser (WebAssembly)

```sh
npm install @pileoni/watermask-wasm
```

The page fetches the tiles; watermask reads them.

```js
import init, { Water, zoomFor, tilesFor, tileUrl } from '@pileoni/watermask-wasm'

await init()
const [w, s, e, n] = [-70.85, 41.3, -70.45, 41.55]
const tilejson = await (await fetch('https://tiles.openfreemap.org/planet')).json()
const z = zoomFor(w, s, e, n, 1200, 256, tilejson.maxzoom)
const ids = tilesFor(w, s, e, n, z)                 // z, x, y, z, x, y, …
const water = new Water()
await Promise.all(Array.from({ length: ids.length / 3 }, async (_, i) => {
  const [tz, tx, ty] = ids.slice(3 * i, 3 * i + 3)
  const r = await fetch(tileUrl(tilejson.tiles[0], tz, tx, ty))
  if (r.ok) water.addTile(tz, tx, ty, new Uint8Array(await r.arrayBuffer()))
}))
const mask = water.mask(w, s, e, n, 1200)
const json = water.geojson(false, [w, s, e, n])      // pieces?, bounds?
```

`demo/index.html` does this on a canvas: build the package into `demo/pkg`
(below), then serve the folder (`python3 -m http.server -d demo`).

## Building

```sh
cargo test                                   # core, offline (real tiles in tests/fixtures)
maturin develop --release && pytest          # Python, in a virtualenv
cd node && npm install && npm run build && npm test
wasm-pack build crates/watermask-wasm --release --target web --out-dir ../../demo/pkg
```

## Limits

- Masks are Web Mercator unless you project yourself (`mask_with`, Rust only).
- Waterway lines are cut to the box but not joined: a river is one line per
  tile it crosses.
- The union snaps coordinates to a grid of about 2³⁰ steps across the area:
  under a millimetre for a city, a few centimetres for the whole world.
- Water covered by something else (`covered=yes`) is not in the tiles.
- Across the 180° meridian, tiles are fetched correctly; GeoJSON longitudes
  on the far side run past 180.

## License

MIT.
