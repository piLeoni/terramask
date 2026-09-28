//! Python bindings: the `watermask._watermask` extension module. The public
//! API lives in `python/watermask/__init__.py`; arrays cross as little-endian
//! bytes so one abi3 wheel serves every Python version without numpy at build
//! time.

use pyo3::exceptions::{PyConnectionError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::path::PathBuf;

type Bounds = (f64, f64, f64, f64);

fn py_err(e: watermask::Error) -> PyErr {
    match e {
        watermask::Error::Input(m) => PyValueError::new_err(m),
        watermask::Error::Net(m) => PyConnectionError::new_err(m),
        watermask::Error::Data(m) => PyRuntimeError::new_err(m),
    }
}

fn arr(b: Bounds) -> [f64; 4] {
    [b.0, b.1, b.2, b.3]
}

fn grid(bounds: Bounds, width: usize, height: Option<usize>) -> PyResult<watermask::Grid> {
    if width == 0 || height == Some(0) {
        return Err(PyValueError::new_err("width and height must be positive"));
    }
    Ok(match height {
        Some(h) => watermask::Grid::new(arr(bounds), width, h),
        None => watermask::Grid::with_width(arr(bounds), width),
    })
}

fn filter(areas: Option<Vec<String>>, lines: Option<Vec<String>>, intermittent: bool, tunnels: bool) -> watermask::Filter {
    let d = watermask::Filter::default();
    watermask::Filter { areas: areas.unwrap_or(d.areas), lines: lines.unwrap_or(d.lines), intermittent, tunnels }
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

/// Water coverage of a grid.
#[pyclass(frozen, module = "watermask._watermask")]
struct Mask {
    inner: watermask::Mask,
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

/// Water gathered from vector tiles.
#[pyclass(module = "watermask._watermask")]
#[derive(Default)]
struct Water {
    inner: watermask::Water,
}

#[pymethods]
impl Water {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    #[pyo3(signature = (z, x, y, data, areas=None, lines=None, intermittent=false, tunnels=false))]
    #[allow(clippy::too_many_arguments)]
    fn add_tile(
        &mut self,
        z: u8,
        x: u32,
        y: u32,
        data: &[u8],
        areas: Option<Vec<String>>,
        lines: Option<Vec<String>>,
        intermittent: bool,
        tunnels: bool,
    ) -> PyResult<()> {
        let f = filter(areas, lines, intermittent, tunnels);
        self.inner.add_tile(watermask::TileId::new(z, x, y), data, &f).map_err(py_err)
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
        let opts = watermask::MaskOptions { supersample, line_width };
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
        let opts = watermask::GeoJsonOptions { pieces, bounds: bounds.map(arr) };
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

#[pyfunction]
#[pyo3(signature = (bounds, width, max_tiles=256, max_zoom=watermask::MAX_ZOOM))]
fn zoom_for(bounds: Bounds, width: usize, max_tiles: usize, max_zoom: u8) -> u8 {
    watermask::zoom_for(&arr(bounds), width, &watermask::ZoomLimits { max_zoom, max_tiles })
}

/// (z, x, y) of the tiles covering the box; x may exceed 2**z - 1 across 180°.
#[pyfunction]
fn tiles_for(bounds: Bounds, zoom: u8) -> Vec<(u8, u32, u32)> {
    watermask::tiles_for(&arr(bounds), zoom).into_iter().map(|t| (t.z, t.x, t.y)).collect()
}

#[pyfunction]
#[pyo3(signature = (bounds, width=None, height=None, zoom=None, areas=None, lines=None, intermittent=false, tunnels=false,
                    source=None, cache=None, no_cache=false, max_tiles=256, log=None))]
#[allow(clippy::too_many_arguments)]
fn fetch(
    py: Python<'_>,
    bounds: Bounds,
    width: Option<usize>,
    height: Option<usize>,
    zoom: Option<u8>,
    areas: Option<Vec<String>>,
    lines: Option<Vec<String>>,
    intermittent: bool,
    tunnels: bool,
    source: Option<String>,
    cache: Option<PathBuf>,
    no_cache: bool,
    max_tiles: usize,
    log: Option<Py<PyAny>>,
) -> PyResult<Water> {
    let mut fetcher = watermask::Fetcher::new();
    if let Some(s) = source {
        fetcher.source = s;
    }
    if let Some(c) = cache {
        fetcher.cache = Some(c);
    }
    if no_cache {
        fetcher.cache = None;
    }
    let f = filter(areas, lines, intermittent, tunnels);
    let progress = |done: usize, total: usize| {
        if let Some(cb) = &log {
            Python::attach(|py| {
                let _ = cb.call1(py, (done, total));
            });
        }
    };
    let inner = match (zoom, width) {
        (Some(z), _) => py.detach(|| fetcher.water(arr(bounds), z, &f, progress)),
        (None, Some(w)) => {
            let g = grid(bounds, w, height)?;
            let limits = watermask::ZoomLimits { max_tiles, ..Default::default() };
            py.detach(|| fetcher.water_for(&g, &f, &limits, progress))
        }
        (None, None) => return Err(PyValueError::new_err("give the output width (or a tile zoom)")),
    }
    .map_err(py_err)?;
    Ok(Water { inner })
}

#[pymodule]
fn _watermask(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("OPENFREEMAP", watermask::OPENFREEMAP)?;
    m.add("MAX_ZOOM", watermask::MAX_ZOOM)?;
    let d = watermask::Filter::default();
    m.add("DEFAULT_AREAS", d.areas)?;
    m.add("DEFAULT_LINES", d.lines)?;
    m.add_class::<Water>()?;
    m.add_class::<Mask>()?;
    m.add_function(wrap_pyfunction!(zoom_for, m)?)?;
    m.add_function(wrap_pyfunction!(tiles_for, m)?)?;
    m.add_function(wrap_pyfunction!(fetch, m)?)?;
    Ok(())
}
