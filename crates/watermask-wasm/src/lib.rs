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
    /// `areas` / `lines`: OpenMapTiles classes to keep (defaults when left out).
    #[wasm_bindgen(constructor)]
    pub fn new(areas: Option<Vec<String>>, lines: Option<Vec<String>>, intermittent: Option<bool>, tunnels: Option<bool>) -> Water {
        let d = watermask::Filter::default();
        let filter = watermask::Filter {
            areas: areas.unwrap_or(d.areas),
            lines: lines.unwrap_or(d.lines),
            intermittent: intermittent.unwrap_or(false),
            tunnels: tunnels.unwrap_or(false),
        };
        Water { inner: watermask::Water::new(), filter }
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
    watermask::Filter::default().areas
}

#[wasm_bindgen(js_name = defaultLines)]
pub fn default_lines() -> Vec<String> {
    watermask::Filter::default().lines
}
