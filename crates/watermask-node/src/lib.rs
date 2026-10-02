//! Node.js bindings (N-API): the native addon behind the `watermask` npm package.

use napi::bindgen_prelude::*;
use napi_derive::napi;

fn js_err(e: watermask::Error) -> Error {
    Error::from_reason(e.to_string())
}

fn bounds(b: &[f64]) -> Result<[f64; 4]> {
    b.try_into().map_err(|_| Error::from_reason("bounds must be [west, south, east, north]"))
}

fn grid(b: &[f64], width: u32, height: Option<u32>) -> Result<watermask::Grid> {
    let b = bounds(b)?;
    if width == 0 || height == Some(0) {
        return Err(Error::from_reason("width and height must be positive"));
    }
    Ok(match height {
        Some(h) => watermask::Grid::new(b, width as usize, h as usize),
        None => watermask::Grid::with_width(b, width as usize),
    })
}

/// Which features to keep: `select`, or else water of `areas` and `lines`.
#[napi(object)]
#[derive(Default)]
pub struct FilterOptions {
    /// Presets (water, land, forest, parks…; see `presets()`) and
    /// `layer:class,class` or `layer:*` rules.
    pub select: Option<Vec<String>>,
    /// Water area classes; default ocean, lake, river, dock.
    pub areas: Option<Vec<String>>,
    /// Waterway line classes; default river, canal, stream.
    pub lines: Option<Vec<String>>,
    pub intermittent: Option<bool>,
    pub tunnels: Option<bool>,
}

fn filter(o: Option<&FilterOptions>) -> Result<watermask::Filter> {
    let Some(o) = o else { return Ok(watermask::Filter::default()) };
    let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let f = match &o.select {
        Some(s) if o.areas.is_none() && o.lines.is_none() => watermask::Filter::parse(s).map_err(js_err)?,
        Some(_) => return Err(Error::from_reason("give select, or areas and lines, not both")),
        None => watermask::Filter::water(
            &o.areas.clone().unwrap_or_else(|| owned(watermask::Filter::DEFAULT_AREAS)),
            &o.lines.clone().unwrap_or_else(|| owned(watermask::Filter::DEFAULT_LINES)),
        ),
    };
    Ok(watermask::Filter { intermittent: o.intermittent.unwrap_or(false), tunnels: o.tunnels.unwrap_or(false), ..f })
}

fn fetcher(source: &Option<String>, cache: &Option<String>, no_cache: Option<bool>) -> watermask::Fetcher {
    let mut f = watermask::Fetcher::new();
    if let Some(s) = source {
        f.source = s.clone();
    }
    if let Some(c) = cache {
        f.cache = Some(c.into());
    }
    if no_cache == Some(true) {
        f.cache = None;
    }
    f
}

/// Layer, class and elevation band of an area.
#[napi(object)]
pub struct AreaInfo {
    pub layer: String,
    pub class: String,
    /// Band in metres after `split`; absent before, and at open ends.
    pub low: Option<f64>,
    pub high: Option<f64>,
}

fn pack(lines: &[Vec<[f32; 2]>]) -> (Float32Array, Uint32Array) {
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
    (Float32Array::new(pts), Uint32Array::new(ends))
}

/// Water coverage of a grid.
#[napi]
pub struct Mask {
    inner: watermask::Mask,
}

#[napi]
impl Mask {
    #[napi(getter)]
    pub fn width(&self) -> u32 {
        self.inner.width as u32
    }

    #[napi(getter)]
    pub fn height(&self) -> u32 {
        self.inner.height as u32
    }

    /// Fraction of each pixel that is water, row 0 at the top.
    #[napi]
    pub fn coverage(&self) -> Float32Array {
        Float32Array::new(self.inner.coverage.clone())
    }

    /// Shoreline polylines in pixels, water on the left: interleaved x,y and
    /// end offsets (in points).
    #[napi]
    pub fn outlines(&self) -> (Float32Array, Uint32Array) {
        pack(&self.inner.outlines())
    }

    /// Pixels to the shore: positive in water, negative on land.
    #[napi]
    pub fn distance(&self) -> Float32Array {
        Float32Array::new(self.inner.distance())
    }
}

/// Water gathered from vector tiles.
#[napi]
#[derive(Default)]
pub struct Water {
    inner: watermask::Water,
}

#[napi]
impl Water {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one vector tile (raw or gzipped protobuf).
    #[napi]
    pub fn add_tile(&mut self, z: u32, x: u32, y: u32, data: &[u8], filter_options: Option<FilterOptions>) -> Result<()> {
        let f = filter(filter_options.as_ref())?;
        self.inner.add_tile(watermask::TileId::new(z as u8, x, y), data, &f).map_err(js_err)
    }

    /// The areas and lines matching presets or `layer:class` rules.
    #[napi]
    pub fn subset(&self, select: Vec<String>) -> Result<Water> {
        let f = watermask::Filter::parse(&select).map_err(js_err)?;
        Ok(Water { inner: self.inner.subset(&f) })
    }

    /// Every area cut into elevation bands at `levels` (metres, depths
    /// negative): below the lowest, between each pair, above the highest.
    #[napi]
    pub fn split(&self, elevation: &Elevation, levels: Vec<f64>) -> Result<Water> {
        Ok(Water { inner: self.inner.split(&elevation.inner, &levels).map_err(js_err)? })
    }

    /// The bands of a split that lie within `low`..`high` metres.
    #[napi]
    pub fn within(&self, low: Option<f64>, high: Option<f64>) -> Water {
        Water { inner: self.inner.within(low.unwrap_or(f64::NEG_INFINITY), high.unwrap_or(f64::INFINITY)) }
    }

    #[napi]
    pub fn areas(&self) -> Vec<AreaInfo> {
        let end = |a: &watermask::Area, i: usize| a.elevation.map(|e| e[i]).filter(|v| v.is_finite());
        self.inner
            .areas
            .iter()
            .map(|a| AreaInfo { layer: a.layer.clone(), class: a.class.clone(), low: end(a, 0), high: end(a, 1) })
            .collect()
    }

    /// Coverage on a north-up Web Mercator grid; height follows the box's
    /// shape when left out.
    #[napi]
    pub fn mask(
        &self,
        bounds: Vec<f64>,
        width: u32,
        height: Option<u32>,
        supersample: Option<u32>,
        line_width: Option<f64>,
    ) -> Result<Mask> {
        let g = grid(&bounds, width, height)?;
        let opts = watermask::MaskOptions { supersample: supersample.unwrap_or(4), line_width: line_width.unwrap_or(0.0) };
        Ok(Mask { inner: self.inner.mask(&g, &opts) })
    }

    /// Waterway lines in pixels of the grid (same packing as outlines).
    #[napi]
    pub fn lines(&self, bounds: Vec<f64>, width: u32, height: Option<u32>) -> Result<(Float32Array, Uint32Array)> {
        Ok(pack(&self.inner.lines_on(&grid(&bounds, width, height)?)))
    }

    #[napi]
    pub fn line_classes(&self) -> Vec<String> {
        self.inner.lines.iter().map(|l| l.class.clone()).collect()
    }

    /// GeoJSON FeatureCollection in lon/lat: one MultiPolygon per area class,
    /// joined across tile edges, and the waterway lines.
    #[napi]
    pub fn geojson(&self, options: Option<GeoJsonOptions>) -> Result<String> {
        let o = options.unwrap_or_default();
        let bounds = o.bounds.as_deref().map(bounds).transpose()?;
        Ok(self.inner.to_geojson(&watermask::GeoJsonOptions { pieces: o.pieces.unwrap_or(false), bounds }))
    }

    #[napi(getter)]
    pub fn area_count(&self) -> u32 {
        self.inner.areas.len() as u32
    }

    #[napi(getter)]
    pub fn line_count(&self) -> u32 {
        self.inner.lines.len() as u32
    }
}

#[napi(object)]
#[derive(Default)]
pub struct GeoJsonOptions {
    /// Keep areas as the pieces the tiles cut them into.
    pub pieces: Option<bool>,
    /// Cut everything to [west, south, east, north].
    pub bounds: Option<Vec<f64>>,
}

#[napi(object)]
#[derive(Default)]
pub struct FetchOptions {
    /// Output width in pixels: picks the tile zoom.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Tile zoom, instead of width.
    pub zoom: Option<u32>,
    /// Presets and `layer:class` rules, instead of areas and lines.
    pub select: Option<Vec<String>>,
    pub areas: Option<Vec<String>>,
    pub lines: Option<Vec<String>>,
    pub intermittent: Option<bool>,
    pub tunnels: Option<bool>,
    /// TileJSON URL or {z}/{x}/{y} template; default OpenFreeMap.
    pub source: Option<String>,
    /// Cache directory; default $WATERMASK_CACHE or the platform's cache folder.
    pub cache: Option<String>,
    pub no_cache: Option<bool>,
    pub max_tiles: Option<u32>,
}

/// Download (or read from the cache) the water in a box, or what `select`
/// names. Blocks until done.
#[napi]
pub fn fetch(bounds: Vec<f64>, options: Option<FetchOptions>) -> Result<Water> {
    let o = options.unwrap_or_default();
    let fetcher = fetcher(&o.source, &o.cache, o.no_cache);
    let f = filter(Some(&FilterOptions {
        select: o.select.clone(),
        areas: o.areas.clone(),
        lines: o.lines.clone(),
        intermittent: o.intermittent,
        tunnels: o.tunnels,
    }))?;
    let inner = match (o.zoom, o.width) {
        (Some(z), _) => fetcher.water(self::bounds(&bounds)?, z as u8, &f, |_, _| {}),
        (None, Some(w)) => {
            let g = grid(&bounds, w, o.height)?;
            let limits = watermask::ZoomLimits { max_tiles: o.max_tiles.unwrap_or(256) as usize, ..Default::default() };
            fetcher.water_for(&g, &f, &limits, |_, _| {})
        }
        (None, None) => return Err(Error::from_reason("give options.width (or options.zoom)")),
    }
    .map_err(js_err)?;
    Ok(Water { inner })
}

/// Terrain heights in metres (sea floor negative) from elevation tiles.
#[napi]
#[derive(Default)]
pub struct Elevation {
    inner: watermask::Elevation,
}

#[napi]
impl Elevation {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one Terrarium PNG. Tiles must share one zoom.
    #[napi]
    pub fn add_tile(&mut self, z: u32, x: u32, y: u32, data: &[u8]) -> Result<()> {
        self.inner.add_tile(watermask::TileId::new(z as u8, x, y), data).map_err(js_err)
    }

    /// Metres at the pixel centres of the grid, row 0 at the top; NaN
    /// outside the tiles.
    #[napi]
    pub fn grid(&self, bounds: Vec<f64>, width: u32, height: Option<u32>) -> Result<Float32Array> {
        Ok(Float32Array::new(self.inner.on(&grid(&bounds, width, height)?)))
    }

    #[napi]
    pub fn at(&self, lon: f64, lat: f64) -> f64 {
        self.inner.at(lon, lat) as f64
    }

    #[napi(getter)]
    pub fn tile_count(&self) -> u32 {
        self.inner.tile_count() as u32
    }
}

#[napi(object)]
#[derive(Default)]
pub struct ElevationOptions {
    /// Output width in pixels: picks the tile zoom.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Tile zoom, instead of width.
    pub zoom: Option<u32>,
    /// Deepest zoom picked from width; default 10, which keeps the sea floor
    /// everywhere (deeper, some coasts flatten the sea to 0 m). Up to 15 for
    /// detailed land heights.
    pub max_zoom: Option<u32>,
    /// {z}/{x}/{y} template of Terrarium PNGs; default AWS Open Data.
    pub source: Option<String>,
    pub cache: Option<String>,
    pub no_cache: Option<bool>,
    pub max_tiles: Option<u32>,
}

/// Download (or read from the cache) terrain for a box. Blocks until done.
#[napi]
pub fn fetch_elevation(bounds: Vec<f64>, options: Option<ElevationOptions>) -> Result<Elevation> {
    let o = options.unwrap_or_default();
    let mut fetcher = fetcher(&None, &o.cache, o.no_cache);
    if let Some(s) = &o.source {
        fetcher.elevation_source = s.clone();
    }
    if let Some(z) = o.max_zoom {
        fetcher.elevation_max_zoom = z as u8;
    }
    let inner = match (o.zoom, o.width) {
        (Some(z), _) => fetcher.elevation(self::bounds(&bounds)?, z as u8, |_, _| {}),
        (None, Some(w)) => {
            let g = grid(&bounds, w, o.height)?;
            let limits = watermask::ZoomLimits { max_tiles: o.max_tiles.unwrap_or(256) as usize, ..Default::default() };
            fetcher.elevation_for(&g, &limits, |_, _| {})
        }
        (None, None) => return Err(Error::from_reason("give options.width (or options.zoom)")),
    }
    .map_err(js_err)?;
    Ok(Elevation { inner })
}

/// Preset names and the rules they stand for.
#[napi]
pub fn presets() -> std::collections::HashMap<String, String> {
    watermask::PRESETS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Tile zoom with at least the detail of `width` pixels across the box.
#[napi]
pub fn zoom_for(bounds: Vec<f64>, width: u32, max_tiles: Option<u32>, max_zoom: Option<u32>) -> Result<u32> {
    let limits =
        watermask::ZoomLimits { max_zoom: max_zoom.map_or(watermask::MAX_ZOOM, |z| z as u8), max_tiles: max_tiles.unwrap_or(256) as usize };
    Ok(watermask::zoom_for(&self::bounds(&bounds)?, width as usize, &limits) as u32)
}

/// Tiles covering the box as [z, x, y] (x may exceed 2^z - 1 across 180°).
#[napi]
pub fn tiles_for(bounds: Vec<f64>, zoom: u32) -> Result<Vec<Vec<u32>>> {
    Ok(watermask::tiles_for(&self::bounds(&bounds)?, zoom as u8).into_iter().map(|t| vec![t.z as u32, t.x, t.y]).collect())
}

#[napi]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
