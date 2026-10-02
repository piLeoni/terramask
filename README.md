# terramask

Geographic masks for any area on Earth: the sea, lakes and rivers, the land,
forests, glaciers, parks, or any layer of OpenStreetMap, as a coverage grid,
outline polylines, a signed distance field or GeoJSON polygons. Areas can also
be cut into depth or height bands from terrain tiles.

Everything comes from tiles fetched on demand, so there is nothing to download
in advance: only the tiles covering the area, at the zoom that matches the
output resolution, and only the layers asked for. Terrain is fetched only when
bands are asked for.

One Rust core, published for Rust, Python, Node.js and the browser (WebAssembly).
terramask grew out of [watermask](https://github.com/piLeoni/watermask), which
did the water only; see [Coming from watermask](#coming-from-watermask).

![Martha's Vineyard: the sea in depth zones, the land in heights, the woods in green](https://raw.githubusercontent.com/piLeoni/terramask/v0.1.0/docs/vineyard-zones.png)

<sub>Martha's Vineyard. The sea from OpenStreetMap, cut into zones at 5, 10, 20 and
30 m deep by terrain tiles; the land cut at 15, 30 and 50 m; the woods from the
`landcover` layer. The shoreline stays OpenStreetMap's: the terrain only decides
where one zone meets the next.</sub>

```python
import terramask

vineyard = (-70.85, 41.3, -70.45, 41.55)          # west, south, east, north
water = terramask.fetch(vineyard, width=1200)     # tiles are cached after the first run
mask = water.mask(vineyard, 1200)

mask.coverage      # (1000, 1200) float32, fraction of each pixel that is water
mask.outlines()    # shoreline: [(n, 2) float32] in pixels, water on the left
mask.distance()    # pixels to the shore, positive in water, negative on land
water.geojson()    # the water as GeoJSON in lon/lat, one MultiPolygon per class

land = terramask.fetch(vineyard, width=1200, select="land")
woods = terramask.fetch(vineyard, width=1200, select=["forest", "parks"])
```

## Water

Water is the default: the sea, lakes, rivers, canals and docks.

![Cape Cod, Martha's Vineyard and Nantucket: the shoreline, and lines following it out to sea](https://raw.githubusercontent.com/piLeoni/terramask/v0.1.0/docs/cape-cod.png)

<sub>Cape Cod and the islands. The shoreline is `mask.outlines()`; the lines at sea are
outlines of `mask.distance()` at growing distances, fading out.</sub>

<p>
<img src="https://raw.githubusercontent.com/piLeoni/terramask/v0.1.0/docs/amsterdam.png" width="32%" alt="Amsterdam: the canal ring and the IJ">
<img src="https://raw.githubusercontent.com/piLeoni/terramask/v0.1.0/docs/venice.png" width="32%" alt="Venice and its lagoon">
<img src="https://raw.githubusercontent.com/piLeoni/terramask/v0.1.0/docs/stockholm.png" width="32%" alt="Stockholm: islands between lake Mälaren and the Baltic">
</p>

<sub>Amsterdam, Venice, Stockholm: `mask.coverage` in grey, the shoreline in black,
waterway centre lines in grey. `cargo run --release --example readme` redraws
every image here from live tiles.</sub>

Two layers of the tiles are read:

| layer      | kind     | classes (default in **bold**)                                         |
|------------|----------|-----------------------------------------------------------------------|
| `water`    | areas    | **ocean**, **lake**, **river**, **dock**, pond, swimming_pool        |
| `waterway` | lines    | **river**, **canal**, **stream**, ditch, drain                        |

`river` areas are the water surface of wide rivers and canals; `river` lines
are centre lines, of every river, however narrow. Seasonal water
(`intermittent`) and water underground (`tunnels`) are left out unless asked
for.

Deriving water from elevation (everything at or below 0 m) misses lakes and
rivers, which sit above sea level, and floods land that lies below it: the
Netherlands, deltas, reclaimed coasts. Complete coastline and water datasets
exist (the OSM water polygons), but they are gigabytes and have to be tiled
and stored before use. Vector tile services already serve that data cut into
tiles, for free, one small tile at a time.

## Land, forests, parks and other layers

`select` picks other layers by preset or by rule; everything else (masks,
outlines, distance, GeoJSON) works the same:

```python
import terramask

vineyard = (-70.85, 41.3, -70.45, 41.55)
land = terramask.fetch(vineyard, width=1200, select="land")             # what the sea leaves
green = terramask.fetch(vineyard, width=1200, select=["forest", "parks"])
green.subset("forest")                                                  # one of the two
terramask.fetch(vineyard, width=1200, select=["landuse:cemetery", "park:*"])   # layer:class rules
```

| preset     | rules                                                     |
|------------|-----------------------------------------------------------|
| `water`    | `water:ocean,lake,river,dock waterway:river,canal,stream` (the default) |
| `ocean`    | `water:ocean`                                             |
| `lakes`    | `water:lake`                                              |
| `rivers`   | `water:river waterway:river,stream`                       |
| `land`     | each tile less its `ocean`                                |
| `forest`, `glacier`, `wetland`, `sand`, `rock`, `grass`, `farmland` | `landcover:wood`, `:ice`, `:wetland`… |
| `parks`    | `park:*` (national parks, nature reserves, protected areas) |
| `urban`    | `landuse:residential,commercial,industrial,retail`        |
| `buildings`| `building:*`                                              |
| `roads`    | `transportation:motorway,trunk,primary,secondary,tertiary,minor,service,busway,raceway` |
| `paths`    | `transportation:path,track`                               |
| `rail`     | `transportation:rail,transit`                             |

Any layer and class of the [OpenMapTiles schema](https://openmaptiles.org/schema/)
can be named as `layer:class,class`, or `layer:*` for all of it. A name picks
a class or a subclass: city parks are `landcover:park` (subclass `park` of
class `grass`), footways `transportation:footway`. Only the layers asked for
are downloaded and cached, each on its own, so asking for forests after water
fetches the tiles once more, and only their `landcover`. GeoJSON features
carry `layer`, `class`, `subclass` and the other attributes of the tile
(`brunnel`, `render_height`…; names in other languages are left out).

Tiles cut every feature at their edges. `merged` joins the pieces of each
class into one shape, which suits masks and coastlines; `joined` mends each
feature on its own, so two buildings sharing a wall stay two, and strings a
road's pieces back into one line across the tile edges.

## Depth and height bands

Terrain tiles give heights, the sea floor below zero. They are fetched only by
`fetch_elevation`; then `split` cuts every area at the levels given, and the
bands are areas like any other:

```python
import terramask

vineyard = (-70.85, 41.3, -70.45, 41.55)
sea = terramask.fetch(vineyard, width=1200, select="ocean")
terrain = terramask.fetch_elevation(vineyard, width=1200)
zones = sea.split(terrain, [-30, -20, -10, -5])   # metres, depths negative
zones.areas            # [AreaInfo(layer='water', cls='ocean', low=None, high=-30.0), …]
deep = zones.within(high=-20).mask(vineyard, 1200)     # deeper than 20 m
zones.geojson()        # properties: layer, class, min, max (null at the open ends)
terrain.grid(vineyard, 1200)                       # (1000, 1200) float32 metres
```

Each area is joined into one polygon, cut to the extent of the terrain tiles,
then into bands: below the lowest level, between each pair, above the
highest. The bands come from contours of the terrain (marching squares, closed
at the edge of the tiles) and are intersected with the area, so together they
cover it exactly and do not overlap. Where the terrain puts sea above sea
level, as it can near a shore, those bits fall in the highest band.

By default the zoom stops at 10 (about 150 m per pixel at the equator), as
the sea floor has no more detail deeper. For detailed land heights pass
`max_zoom` up to 15. From 11 on, some coasts, much of the US for one, come
from land surveys that flatten the sea to 0 m; the fetcher puts the sea floor
back there from the zoom-10 tiles (in Rust, `Elevation::fill_sea` does it for
tiles you read yourself). For terrain magnified past its pixels,
`Elevation::cubic(true)` samples with Catmull-Rom instead of bilinear, so
slopes have no creases at pixel edges.

## Where the data comes from

**Map layers.** By default [OpenFreeMap](https://openfreemap.org): the whole
planet in the [OpenMapTiles schema](https://openmaptiles.org/schema/), rebuilt
weekly from OpenStreetMap, free with no key and no request limits. Any source
in that schema works (MapTiler, a self-hosted copy of OpenFreeMap's planet
file): pass its TileJSON URL or a `{z}/{x}/{y}` URL template.

At zoom 14, the deepest, a tile is about 2.4 km across at the equator and its
coordinates are quantised to about 0.6 m; OpenMapTiles simplifies geometry
there by less than that, so this is full OpenStreetMap detail. At lower zooms
tiles are simplified and small features dropped, in step with the pixel size.

**Terrain.** [Terrain Tiles](https://registry.opendata.aws/terrain-tiles/)
on AWS Open Data (Terrarium PNG, no key, CORS open), from many sources: see
[their attribution](https://github.com/tilezen/joerd/blob/master/docs/attribution.md).
Any Terrarium source works: pass a `{z}/{x}/{y}` URL template.

**Attribution.** Maps made with the map layers must credit
"© OpenMapTiles © OpenStreetMap contributors"; maps with terrain bands, the
terrain sources too.

## How it works

1. **Zoom.** The shallowest zoom whose tiles (drawn 256 px wide) have pixels
   no larger than the output's, capped at the source's deepest zoom, then
   lowered while the area needs more than `max_tiles` tiles (default 256).
2. **Tiles.** Fetched in parallel and cached on disk, only the layers asked
   for, one file each (for water, a fifth of the bytes or less). The cache is `$TERRAMASK_CACHE`, or else
   `~/Library/Caches/terramask` on macOS, `%LOCALAPPDATA%\terramask` on
   Windows, `~/.cache/terramask` on Linux. A cached tile is used for 30 days,
   across OpenFreeMap's weekly builds, then downloaded again; when offline,
   older tiles are used anyway. Tiles past 30 days that nobody asks for are
   deleted, checked once a day. The TileJSON is read each run, since
   OpenFreeMap's tile URLs change with every build, and the last good copy is
   used when offline.
3. **Decoding.** A small protobuf reader pulls the layers out of each tile
   and cuts every feature to its own tile, so the buffer tiles share with
   their neighbours is not counted twice. Land is each tile's square less
   its sea; a tile with nothing in it is all land, since tile sources leave
   empty tiles out and the sea is always something.
4. **Mask.** Polygons are filled with the nonzero winding rule, so the many
   pieces of one sea join without seams and islands stay holes. Each pixel
   gets its exact covered fraction along the row and four sub-rows down it.
5. **Outlines.** Marching squares at coverage ½, joined into polylines with
   the covered side on the left.
6. **Distance.** An exact Euclidean distance transform, signed.
7. **Polygons.** For GeoJSON, the pieces of each layer and class are joined with a
   polygon union ([i_overlay](https://github.com/iShape-Rust/iOverlay)),
   same nonzero rule as the mask, so the polygons cover what the mask covers.
   The output is OGC-valid (shapely and GEOS take it as is), exteriors
   counter-clockwise and holes clockwise, and can be cut to a box.
8. **Bands.** On request only: terrain tiles are decoded into one height
   field, contoured at each level, and the contours intersected with the
   joined areas (see [Depth and height bands](#depth-and-height-bands)).

Masks are on a north-up Web Mercator grid. In Rust, `Features::mask_with` takes
any projection instead.

## Rust

```toml
[dependencies]
terramask = "0.2"          # default-features = false drops the HTTP client and terrain
```

Features: `fetch` (download and cache tiles) and `dem` (terrain, elevation
bands), both on by default.

```rust
use terramask::{Fetcher, Filter, GeoJsonOptions, Grid, MaskOptions, ZoomLimits};

let bounds = [-70.85, 41.3, -70.45, 41.55];
let grid = Grid::with_width(bounds, 1200);
let water = Fetcher::new().features_for(&grid, &Filter::default(), &ZoomLimits::default(), |done, total| {
    eprintln!("{done}/{total} tiles");
})?;
let mask = water.mask(&grid, &MaskOptions::default());
let shore = mask.outlines();
let dist = mask.distance();

// Any projection: lon, lat → pixel.
let tm = water.mask_with(800, 600, &MaskOptions::default(), |lon, lat| my_projection(lon, lat));

// Polygons: joined per class and cut to the box (Features::merged gives the same as structs).
let json = water.to_geojson(&GeoJsonOptions { bounds: Some(bounds), ..Default::default() });

// Buildings and roads one by one, mended across tile edges, with their attributes.
let town = Fetcher::new().water(bounds, 14, &Filter::parse(&["buildings", "roads"])?, |_, _| {})?.joined(Some(bounds));
let bridges = town.lines.iter().filter(|l| l.tag("brunnel") == Some("bridge"));

// Other layers, and depth bands from terrain fetched only here.
let fetcher = Fetcher::new();
let woods = fetcher.features_for(&grid, &Filter::parse(&["forest", "parks"])?, &ZoomLimits::default(), |_, _| {})?;
let sea = water.subset(&Filter::parse(&["ocean"])?);
let terrain = fetcher.elevation_for(&grid, &ZoomLimits::default(), |_, _| {})?;
let zones = sea.split(&terrain, &[-30.0, -20.0, -10.0, -5.0])?;   // Area::elevation holds each band
let deep = zones.within(f64::NEG_INFINITY, -20.0).mask(&grid, &MaskOptions::default());
```

Without the `fetch` feature, bring tiles from anywhere:

```rust
let mut water = terramask::Features::new();
for id in terramask::tiles_for(&bounds, 13) {
    water.add_tile(id, &bytes_of(id), &Filter::default())?;
}
```

`cargo run --release --example render -- -70.85,41.3,-70.45,41.55 1200 out.png`
draws the mask, shoreline and waterways of an area.

## Python

```sh
pip install terramask
```

The examples above cover most uses. Also:
`terramask.fetch(bounds, width, areas=["ocean"], lines=[], source=..., log=print)`,
`water.lines(bounds, width)` for waterway centre lines as `(class, (n, 2) array)`,
`water.mask(..., line_width=1.5)` to burn them into the mask,
`Features().add_tile(z, x, y, data, select=...)` and `Elevation().add_tile(z, x, y, png)`
for your own tiles, `terrain.at(lon, lat)`, `terramask.PRESETS`,
`zoom_for(bounds, width)` and `tiles_for(bounds, zoom)`.

`water.geojson(bounds=vineyard)` cuts the polygons and lines to the box;
`water.geojson(pieces=True)` keeps the areas as the tiles cut them. Into
shapely:

```python
import terramask
from shapely.geometry import shape

vineyard = (-70.85, 41.3, -70.45, 41.55)
water = terramask.fetch(vineyard, width=1200)
sea = next(shape(f["geometry"]) for f in water.geojson(bounds=vineyard)["features"]
           if f["properties"]["class"] == "ocean")
```

## Node.js

```sh
npm install @pileoni/terramask
```

```js
const tm = require('@pileoni/terramask')

const vineyard = [-70.85, 41.3, -70.45, 41.55]
const water = tm.fetch(vineyard, { width: 1200 })   // blocks until the tiles are in
const mask = water.mask(vineyard, 1200)
mask.coverage()                 // Float32Array, row 0 at the top
const [pts, ends] = mask.outlines()   // x,y pairs; ends[i] = end of line i, in points
mask.distance()                 // Float32Array
water.geojson({ bounds: vineyard })   // string; also { pieces: true }

const green = tm.fetch(vineyard, { width: 1200, select: ['forest', 'parks'] })
const sea = water.subset(['ocean'])
const terrain = tm.fetchElevation(vineyard, { width: 1200 })   // maxZoom: up to 15 for land
const zones = sea.split(terrain, [-30, -20, -10, -5])
zones.areas()                   // [{ layer, class, low, high }, …]
zones.within(undefined, -20).mask(vineyard, 1200)   // deeper than 20 m
```

## Browser (WebAssembly)

```sh
npm install @pileoni/terramask-wasm
```

The page fetches the tiles; terramask reads them.

```js
import init, { Features, zoomFor, tilesFor, tileUrl } from '@pileoni/terramask-wasm'

await init()
const [w, s, e, n] = [-70.85, 41.3, -70.45, 41.55]
const tilejson = await (await fetch('https://tiles.openfreemap.org/planet')).json()
const z = zoomFor(w, s, e, n, 1200, 256, tilejson.maxzoom)
const ids = tilesFor(w, s, e, n, z)                 // z, x, y, z, x, y, …
const water = new Features()
await Promise.all(Array.from({ length: ids.length / 3 }, async (_, i) => {
  const [tz, tx, ty] = ids.slice(3 * i, 3 * i + 3)
  const r = await fetch(tileUrl(tilejson.tiles[0], tz, tx, ty))
  if (r.ok) water.addTile(tz, tx, ty, new Uint8Array(await r.arrayBuffer()))
}))
const mask = water.mask(w, s, e, n, 1200)
const json = water.geojson(false, [w, s, e, n])      // pieces?, bounds?
```

Other layers: `Features.select(['forest', 'land'])` instead of `new Features()`.
Terrain: the same loop over `tilesFor(w, s, e, n, zoomFor(w, s, e, n, 1200, 256, terrariumZoom()))`
with `tileUrl(terrarium(), …)` into `new Elevation()`'s `addTile`, then
`water.split(elevation, [-30, -20, -10])`.

`demo/index.html` does this on a canvas: build the package into `demo/pkg`
(below), then serve the folder (`python3 -m http.server -d demo`).

## Building

```sh
cargo test                                   # core, offline (real tiles in tests/fixtures)
maturin develop --release && pytest          # Python, in a virtualenv
cd node && npm install && npm run build && npm test
wasm-pack build crates/terramask-wasm --release --target web --out-dir ../../demo/pkg
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
- Landcover and parks at low zooms are generalised by OpenMapTiles much more
  than water; at a few kilometres per pixel small woods drop out.
- The `boundary` layer holds lines only, so countries and regions are not
  areas here.
- Elevation bands are only as good as the terrain: the sea floor is about a
  kilometre per sample offshore, and terrain tiles of one zoom are read at a
  time.

## Coming from watermask

terramask is watermask 0.2 plus other layers, land and elevation bands, under
a new name. With the default selection it returns the same water. To move
over, change the package name, then:

| watermask                                   | terramask                          |
|---------------------------------------------|------------------------------------|
| `Water` (Rust, Python, Node, wasm)          | `Features`                         |
| `Fetcher::water_for` (Rust)                 | `Fetcher::features_for`            |
| `water_fraction` on masks (Rust, Python)    | `fraction`                         |
| `Filter { areas, lines, .. }` (Rust)        | `Filter::water(&areas, &lines)`, or `Filter::parse` |
| `$WATERMASK_CACHE`, `…/watermask` cache folder | `$TERRAMASK_CACHE`, `…/terramask` |

The `areas` and `lines` options of the other languages are unchanged. The
cache is a new folder, so the first run downloads the tiles again.

## License

MIT.
