//! Python bindings: the `terramask._terramask` extension module. The public
//! API lives in `python/terramask/__init__.py`; arrays cross as little-endian
//! bytes so one abi3 wheel serves every Python version without numpy at build
//! time.

use pyo3::exceptions::{PyConnectionError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::path::PathBuf;

type Bounds = (f64, f64, f64, f64);

fn py_err(e: terramask::Error) -> PyErr {
    match e {
        terramask::Error::Input(m) => PyValueError::new_err(m),
        terramask::Error::Net(m) => PyConnectionError::new_err(m),
        terramask::Error::Data(m) => PyRuntimeError::new_err(m),
    }
}

fn arr(b: Bounds) -> [f64; 4] {
    [b.0, b.1, b.2, b.3]
}

fn grid(bounds: Bounds, width: usize, height: Option<usize>) -> PyResult<terramask::Grid> {
    if width == 0 || height == Some(0) {
        return Err(PyValueError::new_err("width and height must be positive"));
    }
    Ok(match height {
        Some(h) => terramask::Grid::new(arr(bounds), width, h),
        None => terramask::Grid::with_width(arr(bounds), width),
    })
}

/// `select` (presets and layer:class rules), or else water of `areas` and `lines`.
fn filter(
    select: Option<Vec<String>>,
    areas: Option<Vec<String>>,
    lines: Option<Vec<String>>,
    intermittent: bool,
    tunnels: bool,
) -> PyResult<terramask::Filter> {
    let f = match select {
        Some(s) if areas.is_none() && lines.is_none() => terramask::Filter::parse(&s).map_err(py_err)?,
        Some(_) => return Err(PyValueError::new_err("give select, or areas and lines, not both")),
        None => terramask::Filter::water(
            &areas.unwrap_or_else(|| terramask::Filter::DEFAULT_AREAS.iter().map(|s| s.to_string()).collect()),
            &lines.unwrap_or_else(|| terramask::Filter::DEFAULT_LINES.iter().map(|s| s.to_string()).collect()),
        ),
    };
    Ok(terramask::Filter { intermittent, tunnels, ..f })
}

fn fetcher(source: Option<String>, cache: Option<PathBuf>, no_cache: bool) -> terramask::Fetcher {
    let mut f = terramask::Fetcher::new();
    if let Some(c) = cache {
        f.cache = Some(c);
    }
    if no_cache {
        f.cache = None;
    }
    if let Some(s) = source {
        f.source = s;
    }
    f
}

fn progress(log: &Option<Py<PyAny>>) -> impl Fn(usize, usize) + Sync + '_ {
    move |done: usize, total: usize| {
        if let Some(cb) = log {
            Python::attach(|py| {
                let _ = cb.call1(py, (done, total));
            });
        }
    }
}

fn f32s<'py>(py: Python<'py>, v: &[f32]) -> Bound<'py, PyBytes> {
    PyBytes::new(py, &v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
}

/// Polylines as (x,y f32 pairs, u32 end offsets in points).
fn pack<'py>(py: Python<'py>, lines: &[Vec<[f32; 2]>]) -> (Bound<'py, PyBytes>, Bound<'py, PyBytes>) {
    let mut pts = Vec::with_capacity(lines.iter().map(|l| l.len() * 8).sum());
    let mut ends = Vec::with_capacity(lines.len() * 4);
    let mut n = 0u32;
    for l in lines {
        for p in l {
            pts.extend_from_slice(&p[0].to_le_bytes());
            pts.extend_from_slice(&p[1].to_le_bytes());
        }
        n += l.len() as u32;
        ends.extend_from_slice(&n.to_le_bytes());
    }
    (PyBytes::new(py, &pts), PyBytes::new(py, &ends))
}

/// Coverage of a grid.
#[pyclass(frozen, module = "terramask._terramask")]
struct Mask {
    inner: terramask::Mask,
}

#[pymethods]
impl Mask {
    #[getter]
    fn width(&self) -> usize {
        self.inner.width
    }
    #[getter]
    fn height(&self) -> usize {
        self.inner.height
    }
    fn coverage<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        f32s(py, &self.inner.coverage)
    }
    fn outlines<'py>(&self, py: Python<'py>) -> (Bound<'py, PyBytes>, Bound<'py, PyBytes>) {
        let o = py.detach(|| self.inner.outlines());
        pack(py, &o)
    }
    fn distance<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let d = py.detach(|| self.inner.distance());
        f32s(py, &d)
    }
}

/// Areas and lines gathered from vector tiles.
#[pyclass(module = "terramask._terramask")]
#[derive(Default)]
struct Features {
    inner: terramask::Features,
}

#[pymethods]
impl Features {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    #[pyo3(signature = (z, x, y, data, select=None, areas=None, lines=None, intermittent=false, tunnels=false))]
    #[allow(clippy::too_many_arguments)]
    fn add_tile(
        &mut self,
        z: u8,
        x: u32,
        y: u32,
        data: &[u8],
        select: Option<Vec<String>>,
        areas: Option<Vec<String>>,
        lines: Option<Vec<String>>,
        intermittent: bool,
        tunnels: bool,
    ) -> PyResult<()> {
        let f = filter(select, areas, lines, intermittent, tunnels)?;
        self.inner.add_tile(terramask::TileId::new(z, x, y), data, &f).map_err(py_err)
    }

    fn subset(&self, select: Vec<String>) -> PyResult<Features> {
        let f = terramask::Filter::parse(&select).map_err(py_err)?;
        Ok(Features { inner: self.inner.subset(&f) })
    }

    fn within(&self, low: f64, high: f64) -> Features {
        Features { inner: self.inner.within(low, high) }
    }

    fn split(&self, py: Python<'_>, elevation: &Elevation, levels: Vec<f64>) -> PyResult<Features> {
        let inner = py.detach(|| self.inner.split(&elevation.inner, &levels)).map_err(py_err)?;
        Ok(Features { inner })
    }

    /// (layer, class, low, high) of each area; low and high are None until
    /// split, and at the open ends of the bands.
    fn area_info(&self) -> Vec<(String, String, Option<f64>, Option<f64>)> {
        let end = |a: &terramask::Area, i: usize| a.elevation.map(|e| e[i]).filter(|v| v.is_finite());
        self.inner.areas.iter().map(|a| (a.layer.clone(), a.class.clone(), end(a, 0), end(a, 1))).collect()
    }

    #[pyo3(signature = (bounds, width, height=None, supersample=4, line_width=0.0))]
    fn mask(
        &self,
        py: Python<'_>,
        bounds: Bounds,
        width: usize,
        height: Option<usize>,
        supersample: u32,
        line_width: f64,
    ) -> PyResult<Mask> {
        let g = grid(bounds, width, height)?;
        let opts = terramask::MaskOptions { supersample, line_width };
        Ok(Mask { inner: py.detach(|| self.inner.mask(&g, &opts)) })
    }

    /// Waterway lines in pixels of the grid, and their classes.
    #[pyo3(signature = (bounds, width, height=None))]
    fn lines<'py>(
        &self,
        py: Python<'py>,
        bounds: Bounds,
        width: usize,
        height: Option<usize>,
    ) -> PyResult<(Bound<'py, PyBytes>, Bound<'py, PyBytes>, Vec<String>)> {
        let g = grid(bounds, width, height)?;
        let (pts, ends) = pack(py, &self.inner.lines_on(&g));
        Ok((pts, ends, self.inner.lines.iter().map(|l| l.class.clone()).collect()))
    }

    #[pyo3(signature = (pieces=false, bounds=None))]
    fn geojson(&self, py: Python<'_>, pieces: bool, bounds: Option<Bounds>) -> String {
        let opts = terramask::GeoJsonOptions { pieces, bounds: bounds.map(arr) };
        py.detach(|| self.inner.to_geojson(&opts))
    }

    #[getter]
    fn area_count(&self) -> usize {
        self.inner.areas.len()
    }

    #[getter]
    fn line_count(&self) -> usize {
        self.inner.lines.len()
    }
}

/// Terrain heights from elevation tiles.
#[pyclass(module = "terramask._terramask")]
#[derive(Default)]
struct Elevation {
    inner: terramask::Elevation,
}

#[pymethods]
impl Elevation {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    /// A Terrarium PNG.
    fn add_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> PyResult<()> {
        self.inner.add_tile(terramask::TileId::new(z, x, y), data).map_err(py_err)
    }

    #[pyo3(signature = (bounds, width, height=None))]
    fn grid<'py>(&self, py: Python<'py>, bounds: Bounds, width: usize, height: Option<usize>) -> PyResult<(Bound<'py, PyBytes>, usize)> {
        let g = grid(bounds, width, height)?;
        let h = py.detach(|| self.inner.on(&g));
        Ok((f32s(py, &h), g.height))
    }

    fn at(&self, lon: f64, lat: f64) -> f32 {
        self.inner.at(lon, lat)
    }

    #[getter]
    fn tile_count(&self) -> usize {
        self.inner.tile_count()
    }
}

#[pyfunction]
#[pyo3(signature = (bounds, width=None, height=None, zoom=None, max_zoom=None, source=None, cache=None, no_cache=false,
                    max_tiles=256, log=None))]
#[allow(clippy::too_many_arguments)]
fn fetch_elevation(
    py: Python<'_>,
    bounds: Bounds,
    width: Option<usize>,
    height: Option<usize>,
    zoom: Option<u8>,
    max_zoom: Option<u8>,
    source: Option<String>,
    cache: Option<PathBuf>,
    no_cache: bool,
    max_tiles: usize,
    log: Option<Py<PyAny>>,
) -> PyResult<Elevation> {
    let mut f = fetcher(None, cache, no_cache);
    if let Some(s) = source {
        f.elevation_source = s;
    }
    if let Some(z) = max_zoom {
        f.elevation_max_zoom = z;
    }
    let progress = progress(&log);
    let inner = match (zoom, width) {
        (Some(z), _) => py.detach(|| f.elevation(arr(bounds), z, progress)),
        (None, Some(w)) => {
            let g = grid(bounds, w, height)?;
            let limits = terramask::ZoomLimits { max_tiles, ..Default::default() };
            py.detach(|| f.elevation_for(&g, &limits, progress))
        }
        (None, None) => return Err(PyValueError::new_err("give the output width (or a tile zoom)")),
    }
    .map_err(py_err)?;
    Ok(Elevation { inner })
}

#[pyfunction]
#[pyo3(signature = (bounds, width, max_tiles=256, max_zoom=terramask::MAX_ZOOM))]
fn zoom_for(bounds: Bounds, width: usize, max_tiles: usize, max_zoom: u8) -> u8 {
    terramask::zoom_for(&arr(bounds), width, &terramask::ZoomLimits { max_zoom, max_tiles })
}

/// (z, x, y) of the tiles covering the box; x may exceed 2**z - 1 across 180°.
#[pyfunction]
fn tiles_for(bounds: Bounds, zoom: u8) -> Vec<(u8, u32, u32)> {
    terramask::tiles_for(&arr(bounds), zoom).into_iter().map(|t| (t.z, t.x, t.y)).collect()
}

#[pyfunction]
#[pyo3(signature = (bounds, width=None, height=None, zoom=None, select=None, areas=None, lines=None, intermittent=false,
                    tunnels=false, source=None, cache=None, no_cache=false, max_tiles=256, log=None))]
#[allow(clippy::too_many_arguments)]
fn fetch(
    py: Python<'_>,
    bounds: Bounds,
    width: Option<usize>,
    height: Option<usize>,
    zoom: Option<u8>,
    select: Option<Vec<String>>,
    areas: Option<Vec<String>>,
    lines: Option<Vec<String>>,
    intermittent: bool,
    tunnels: bool,
    source: Option<String>,
    cache: Option<PathBuf>,
    no_cache: bool,
    max_tiles: usize,
    log: Option<Py<PyAny>>,
) -> PyResult<Features> {
    let fetcher = fetcher(source, cache, no_cache);
    let f = filter(select, areas, lines, intermittent, tunnels)?;
    let progress = progress(&log);
    let inner = match (zoom, width) {
        (Some(z), _) => py.detach(|| fetcher.water(arr(bounds), z, &f, progress)),
        (None, Some(w)) => {
            let g = grid(bounds, w, height)?;
            let limits = terramask::ZoomLimits { max_tiles, ..Default::default() };
            py.detach(|| fetcher.features_for(&g, &f, &limits, progress))
        }
        (None, None) => return Err(PyValueError::new_err("give the output width (or a tile zoom)")),
    }
    .map_err(py_err)?;
    Ok(Features { inner })
}

#[pymodule]
fn _terramask(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("OPENFREEMAP", terramask::OPENFREEMAP)?;
    m.add("MAX_ZOOM", terramask::MAX_ZOOM)?;
    m.add("TERRARIUM", terramask::TERRARIUM)?;
    m.add("TERRARIUM_ZOOM", terramask::TERRARIUM_ZOOM)?;
    m.add("TERRARIUM_MAX_ZOOM", terramask::TERRARIUM_MAX_ZOOM)?;
    m.add("DEFAULT_AREAS", terramask::Filter::DEFAULT_AREAS.to_vec())?;
    m.add("DEFAULT_LINES", terramask::Filter::DEFAULT_LINES.to_vec())?;
    m.add("PRESETS", terramask::PRESETS.to_vec())?;
    m.add_class::<Features>()?;
    m.add_class::<Mask>()?;
    m.add_class::<Elevation>()?;
    m.add_function(wrap_pyfunction!(zoom_for, m)?)?;
    m.add_function(wrap_pyfunction!(tiles_for, m)?)?;
    m.add_function(wrap_pyfunction!(fetch, m)?)?;
    m.add_function(wrap_pyfunction!(fetch_elevation, m)?)?;
    Ok(())
}
