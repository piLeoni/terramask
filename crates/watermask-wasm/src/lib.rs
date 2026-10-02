//! WebAssembly bindings. The page fetches the tiles (see `tilesFor` and
//! `tileUrl`) and hands their bytes to `Water.addTile`.

use wasm_bindgen::prelude::*;

fn js_err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn pack(lines: &[Vec<[f32; 2]>]) -> js_sys::Array {
    let mut pts = Vec::with_capacity(lines.iter().map(|l| l.len() * 2).sum());
    let mut ends = Vec::with_capacity(lines.len());
    let mut n = 0u32;
    for l in lines {
        for p in l {
            pts.extend_from_slice(p);
        }
        n += l.len() as u32;
        ends.push(n);
    }
    let out = js_sys::Array::new();
    out.push(&js_sys::Float32Array::from(&pts[..]));
    out.push(&js_sys::Uint32Array::from(&ends[..]));
    out
}

fn grid(west: f64, south: f64, east: f64, north: f64, width: u32, height: Option<u32>) -> Result<watermask::Grid, JsError> {
    if width == 0 || height == Some(0) {
        return Err(JsError::new("width and height must be positive"));
    }
    let b = [west, south, east, north];
    Ok(match height {
        Some(h) => watermask::Grid::new(b, width as usize, h as usize),
        None => watermask::Grid::with_width(b, width as usize),
    })
}

/// Water coverage of a grid.
#[wasm_bindgen]
pub struct Mask {
    inner: watermask::Mask,
}

#[wasm_bindgen]
impl Mask {
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.inner.width as u32
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.inner.height as u32
    }

    /// Fraction of each pixel that is water, row 0 at the top.
    pub fn coverage(&self) -> js_sys::Float32Array {
        js_sys::Float32Array::from(&self.inner.coverage[..])
    }

    /// `[points, ends]`: interleaved x,y in pixels and end offsets in points.
    /// Water is on the left of each line.
    pub fn outlines(&self) -> js_sys::Array {
        pack(&self.inner.outlines())
    }

    /// Pixels to the shore: positive in water, negative on land.
    pub fn distance(&self) -> js_sys::Float32Array {
        js_sys::Float32Array::from(&self.inner.distance()[..])
    }
}

/// Water gathered from vector tiles.
#[wasm_bindgen]
pub struct Water {
    inner: watermask::Water,
    filter: watermask::Filter,
}

#[wasm_bindgen]
impl Water {
    /// `areas` / `lines`: water classes to keep (defaults when left out).
    /// For other layers, see `Water.select`.
    #[wasm_bindgen(constructor)]
    pub fn new(areas: Option<Vec<String>>, lines: Option<Vec<String>>, intermittent: Option<bool>, tunnels: Option<bool>) -> Water {
        let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let f = watermask::Filter::water(
            &areas.unwrap_or_else(|| owned(watermask::Filter::DEFAULT_AREAS)),
            &lines.unwrap_or_else(|| owned(watermask::Filter::DEFAULT_LINES)),
        );
        let filter = watermask::Filter { intermittent: intermittent.unwrap_or(false), tunnels: tunnels.unwrap_or(false), ..f };
        Water { inner: watermask::Water::new(), filter }
    }

    /// Keep what `items` names: presets (water, land, forest, parks…; see
    /// `presets()`) and `layer:class,class` or `layer:*` rules.
    pub fn select(items: Vec<String>, intermittent: Option<bool>, tunnels: Option<bool>) -> Result<Water, JsError> {
        let f = watermask::Filter::parse(&items).map_err(js_err)?;
        let filter = watermask::Filter { intermittent: intermittent.unwrap_or(false), tunnels: tunnels.unwrap_or(false), ..f };
        Ok(Water { inner: watermask::Water::new(), filter })
    }

    /// The areas and lines matching presets or rules.
    pub fn subset(&self, items: Vec<String>) -> Result<Water, JsError> {
        let f = watermask::Filter::parse(&items).map_err(js_err)?;
        Ok(Water { inner: self.inner.subset(&f), filter: self.filter.clone() })
    }

    /// Every area cut into elevation bands at `levels` (metres, depths
    /// negative): below the lowest, between each pair, above the highest.
    pub fn split(&self, elevation: &Elevation, levels: Vec<f64>) -> Result<Water, JsError> {
        Ok(Water { inner: self.inner.split(&elevation.inner, &levels).map_err(js_err)?, filter: self.filter.clone() })
    }

    /// The bands of a split that lie within `low`..`high` metres.
    pub fn within(&self, low: Option<f64>, high: Option<f64>) -> Water {
        let inner = self.inner.within(low.unwrap_or(f64::NEG_INFINITY), high.unwrap_or(f64::INFINITY));
        Water { inner, filter: self.filter.clone() }
    }

    /// `{ layer, class, low, high }` of each area; low and high are null
    /// before a split and at the open ends of the bands.
    pub fn areas(&self) -> js_sys::Array {
        let out = js_sys::Array::new();
        for a in &self.inner.areas {
            let o = js_sys::Object::new();
            let end = |i: usize| a.elevation.map(|e| e[i]).filter(|v| v.is_finite()).map_or(JsValue::NULL, JsValue::from);
            for (k, v) in [("layer", JsValue::from(&a.layer)), ("class", JsValue::from(&a.class)), ("low", end(0)), ("high", end(1))] {
                let _ = js_sys::Reflect::set(&o, &JsValue::from(k), &v);
            }
            out.push(&o);
        }
        out
    }

    /// Read one tile's bytes (raw or gzipped protobuf). Use the unwrapped `x`
    /// from `tilesFor`.
    #[wasm_bindgen(js_name = addTile)]
    pub fn add_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<(), JsError> {
        self.inner.add_tile(watermask::TileId::new(z, x, y), data, &self.filter).map_err(js_err)
    }

    /// Coverage on a north-up Web Mercator grid.
    #[allow(clippy::too_many_arguments)]
    pub fn mask(
        &self,
        west: f64,
        south: f64,
        east: f64,
        north: f64,
        width: u32,
        height: Option<u32>,
        supersample: Option<u32>,
        line_width: Option<f64>,
    ) -> Result<Mask, JsError> {
        let g = grid(west, south, east, north, width, height)?;
        let opts = watermask::MaskOptions { supersample: supersample.unwrap_or(4), line_width: line_width.unwrap_or(0.0) };
        Ok(Mask { inner: self.inner.mask(&g, &opts) })
    }

    /// Waterway lines in pixels, packed like `Mask.outlines`.
    pub fn lines(&self, west: f64, south: f64, east: f64, north: f64, width: u32, height: Option<u32>) -> Result<js_sys::Array, JsError> {
        Ok(pack(&self.inner.lines_on(&grid(west, south, east, north, width, height)?)))
    }

    #[wasm_bindgen(js_name = lineClasses)]
    pub fn line_classes(&self) -> Vec<String> {
        self.inner.lines.iter().map(|l| l.class.clone()).collect()
    }

    /// GeoJSON FeatureCollection in lon/lat: one MultiPolygon per area class,
    /// joined across tile edges, and the waterway lines. `pieces` keeps the
    /// areas as the tiles cut them; `bounds` ([west, south, east, north])
    /// cuts everything to that box.
    pub fn geojson(&self, pieces: Option<bool>, bounds: Option<Vec<f64>>) -> Result<String, JsError> {
        let bounds =
            bounds.map(|b| <[f64; 4]>::try_from(b).map_err(|_| JsError::new("bounds must be [west, south, east, north]"))).transpose()?;
        Ok(self.inner.to_geojson(&watermask::GeoJsonOptions { pieces: pieces.unwrap_or(false), bounds }))
    }

    #[wasm_bindgen(getter, js_name = areaCount)]
    pub fn area_count(&self) -> u32 {
        self.inner.areas.len() as u32
    }

    #[wasm_bindgen(getter, js_name = lineCount)]
    pub fn line_count(&self) -> u32 {
        self.inner.lines.len() as u32
    }
}

/// Terrain heights in metres (sea floor negative). The page fetches the
/// tiles (Terrarium PNGs, see `terrarium()`) and hands their bytes here.
#[wasm_bindgen]
#[derive(Default)]
pub struct Elevation {
    inner: watermask::Elevation,
}

#[wasm_bindgen]
impl Elevation {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Elevation {
        Self::default()
    }

    /// One Terrarium PNG. Tiles must share one zoom.
    #[wasm_bindgen(js_name = addTile)]
    pub fn add_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<(), JsError> {
        self.inner.add_tile(watermask::TileId::new(z, x, y), data).map_err(js_err)
    }

    /// Metres at the pixel centres of the grid, row 0 at the top; NaN
    /// outside the tiles.
    #[allow(clippy::too_many_arguments)]
    pub fn grid(
        &self,
        west: f64,
        south: f64,
        east: f64,
        north: f64,
        width: u32,
        height: Option<u32>,
    ) -> Result<js_sys::Float32Array, JsError> {
        Ok(js_sys::Float32Array::from(&self.inner.on(&grid(west, south, east, north, width, height)?)[..]))
    }

    pub fn at(&self, lon: f64, lat: f64) -> f32 {
        self.inner.at(lon, lat)
    }

    #[wasm_bindgen(getter, js_name = tileCount)]
    pub fn tile_count(&self) -> u32 {
        self.inner.tile_count() as u32
    }
}

/// `{z}/{x}/{y}` template of the default terrain tiles (AWS Open Data).
#[wasm_bindgen]
pub fn terrarium() -> String {
    watermask::TERRARIUM.to_string()
}

/// Deepest zoom of the default terrain tiles.
#[wasm_bindgen(js_name = terrariumMaxZoom)]
pub fn terrarium_max_zoom() -> u8 {
    watermask::TERRARIUM_MAX_ZOOM
}

/// Deepest terrain zoom worth reading for the sea floor: deeper, some
/// coasts flatten the sea to 0 m. Use it as `maxZoom` in `zoomFor`.
#[wasm_bindgen(js_name = terrariumZoom)]
pub fn terrarium_zoom() -> u8 {
    watermask::TERRARIUM_ZOOM
}

/// Preset names and the rules they stand for, as an object.
#[wasm_bindgen]
pub fn presets() -> js_sys::Object {
    let o = js_sys::Object::new();
    for (k, v) in watermask::PRESETS {
        let _ = js_sys::Reflect::set(&o, &JsValue::from(*k), &JsValue::from(*v));
    }
    o
}

/// Tile zoom with at least the detail of `width` pixels across the box.
#[wasm_bindgen(js_name = zoomFor)]
pub fn zoom_for(west: f64, south: f64, east: f64, north: f64, width: u32, max_tiles: Option<u32>, max_zoom: Option<u8>) -> u8 {
    let limits = watermask::ZoomLimits { max_zoom: max_zoom.unwrap_or(watermask::MAX_ZOOM), max_tiles: max_tiles.unwrap_or(256) as usize };
    watermask::zoom_for(&[west, south, east, north], width as usize, &limits)
}

/// Tiles covering the box, flat: z, x, y, z, x, y, … (x unwrapped across 180°).
#[wasm_bindgen(js_name = tilesFor)]
pub fn tiles_for(west: f64, south: f64, east: f64, north: f64, zoom: u8) -> Vec<u32> {
    watermask::tiles_for(&[west, south, east, north], zoom).into_iter().flat_map(|t| [t.z as u32, t.x, t.y]).collect()
}

/// A tile's URL from a `{z}/{x}/{y}` template, with x wrapped into range.
#[wasm_bindgen(js_name = tileUrl)]
pub fn tile_url(template: &str, z: u8, x: u32, y: u32) -> String {
    let t = watermask::TileId::new(z, x, y);
    template.replace("{z}", &z.to_string()).replace("{x}", &t.wrapped_x().to_string()).replace("{y}", &y.to_string())
}

#[wasm_bindgen(js_name = defaultAreas)]
pub fn default_areas() -> Vec<String> {
    watermask::Filter::DEFAULT_AREAS.iter().map(|s| s.to_string()).collect()
}

#[wasm_bindgen(js_name = defaultLines)]
pub fn default_lines() -> Vec<String> {
    watermask::Filter::DEFAULT_LINES.iter().map(|s| s.to_string()).collect()
}
