//! Water masks for any area, from OpenStreetMap vector tiles fetched on demand.
//!
//! The sea, lakes, rivers, canals and docks come from the `water` and
//! `waterway` layers of OpenMapTiles-schema vector tiles (OpenFreeMap by
//! default). Only the tiles covering the area are read, at the zoom that
//! matches the output resolution.
//!
//! ```no_run
//! # #[cfg(feature = "fetch")] {
//! use watermask::{Fetcher, Filter, Grid, MaskOptions, ZoomLimits};
//! let bounds = [-70.85, 41.3, -70.45, 41.55]; // west, south, east, north
//! let grid = Grid::with_width(bounds, 1200);
//! let water = Fetcher::new().water_for(&grid, &Filter::default(), &ZoomLimits::default(), |_, _| {})?;
//! let mask = water.mask(&grid, &MaskOptions::default());
//! let shore = mask.outlines();          // polylines in pixels
//! let dist = mask.distance();           // pixels to the shore, + in water
//! # }
//! # Ok::<(), watermask::Error>(())
//! ```
//!
//! Nothing here needs the network: [`Water::add_tile`] takes tile bytes from
//! anywhere. The `fetch` feature adds [`Fetcher`], which downloads and caches
//! them.

mod contour;
mod distance;
#[cfg(feature = "fetch")]
mod fetch;
mod geojson;
mod merge;
mod mvt;
mod raster;
mod tile;

use std::fmt;

#[cfg(feature = "fetch")]
pub use fetch::{default_cache, Fetcher, Source, MAX_AGE, OPENFREEMAP};
pub use tile::{lonlat_to_merc, merc_to_lonlat, tiles_for, zoom_for, Bounds, TileId, MAX_ZOOM};

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Bad arguments.
    Input(String),
    /// Download failed.
    Net(String),
    /// A tile could not be read.
    Data(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Input(m) | Error::Net(m) | Error::Data(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

/// Which features count as water. Classes are those of the OpenMapTiles
/// schema: areas `ocean`, `lake`, `river`, `dock`, `pond`, `swimming_pool`;
/// lines `river`, `canal`, `stream`, `ditch`, `drain`.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub areas: Vec<String>,
    pub lines: Vec<String>,
    /// Keep water that is only there part of the year.
    pub intermittent: bool,
    /// Keep water that runs underground (culverts, covered channels).
    pub tunnels: bool,
}

impl Default for Filter {
    fn default() -> Self {
        let s = |v: &[&str]| v.iter().map(|c| c.to_string()).collect();
        Filter {
            areas: s(&["ocean", "lake", "river", "dock"]),
            lines: s(&["river", "canal", "stream"]),
            intermittent: false,
            tunnels: false,
        }
    }
}

/// A closed ring in Web Mercator metres. Exterior rings bound water, the
/// others are islands in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ring {
    pub exterior: bool,
    pub points: Vec<[f64; 2]>,
}

/// One water area, as cut by its tile.
#[derive(Debug, Clone, PartialEq)]
pub struct Area {
    pub class: String,
    pub rings: Vec<Ring>,
}

/// One waterway centre line, as cut by its tile, in Web Mercator metres.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub class: String,
    pub points: Vec<[f64; 2]>,
}

/// Water collected from tiles. Each tile's features are cut to the tile, so
/// neighbouring tiles meet edge to edge without overlapping; an area spanning
/// several tiles is several pieces.
#[derive(Debug, Clone, Default)]
pub struct Water {
    pub areas: Vec<Area>,
    pub lines: Vec<Line>,
}

impl Water {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one vector tile (raw or gzipped protobuf) and keep the water that
    /// passes `filter`.
    pub fn add_tile(&mut self, id: TileId, bytes: &[u8], filter: &Filter) -> Result<(), Error> {
        let t = mvt::read_water(bytes, filter).map_err(|e| Error::Data(format!("tile {id}: {e}")))?;
        let [x0, _, _, y1] = id.merc_bounds();
        let size = id.merc_size();
        let to_merc = |pts: Vec<[f64; 2]>| pts.into_iter().map(|p| [x0 + p[0] * size, y1 - p[1] * size]).collect();
        for a in t.areas {
            let rings = a.rings.into_iter().map(|r| Ring { exterior: r.exterior, points: to_merc(r.points) }).collect();
            self.areas.push(Area { class: a.class, rings });
        }
        for l in t.lines {
            self.lines.push(Line { class: l.class, points: to_merc(l.points) });
        }
        Ok(())
    }

    /// Water coverage on a north-up Web Mercator grid.
    pub fn mask(&self, grid: &Grid, opts: &MaskOptions) -> Mask {
        let [x0, y0, x1, y1] = grid.merc;
        let (sx, sy) = (grid.width as f64 / (x1 - x0), grid.height as f64 / (y1 - y0));
        self.rasterize(grid.width, grid.height, opts, &|p| [(p[0] - x0) * sx, (y1 - p[1]) * sy])
    }

    /// Water coverage on any grid: `project` takes lon, lat in degrees and
    /// returns the pixel position (x right, y down, pixel centres at +0.5).
    pub fn mask_with(&self, width: usize, height: usize, opts: &MaskOptions, project: impl Fn(f64, f64) -> [f64; 2]) -> Mask {
        self.rasterize(width, height, opts, &|p| {
            let [lon, lat] = merc_to_lonlat(p[0], p[1]);
            project(lon, lat)
        })
    }

    fn rasterize(&self, width: usize, height: usize, opts: &MaskOptions, px: &dyn Fn([f64; 2]) -> [f64; 2]) -> Mask {
        let mut r = raster::Raster::new(width, height);
        for a in &self.areas {
            for ring in &a.rings {
                r.add_ring(ring.points.iter().map(|&p| px(p)).collect(), ring.exterior);
            }
        }
        if opts.line_width > 0.0 {
            for l in &self.lines {
                let pts: Vec<[f64; 2]> = l.points.iter().map(|&p| px(p)).collect();
                r.add_stroke(&pts, opts.line_width);
            }
        }
        Mask { width, height, coverage: r.fill(opts.supersample.max(1)) }
    }

    /// Waterway lines on a Web Mercator grid, in pixels, in the order of
    /// [`Water::lines`].
    pub fn lines_on(&self, grid: &Grid) -> Vec<Vec<[f32; 2]>> {
        self.lines.iter().map(|l| l.points.iter().map(|&p| grid.merc_to_px(p)).collect()).collect()
    }

    /// The areas of each class joined across tile edges into one [`Area`],
    /// each exterior ring followed by its islands; with `bounds`, areas and
    /// lines cut to that box. Lines are not joined.
    pub fn merged(&self, bounds: Option<Bounds>) -> Water {
        self.reshaped(true, bounds)
    }

    /// Areas and lines cut to `bounds`, the tile pieces kept apart.
    pub fn clipped(&self, bounds: Bounds) -> Water {
        self.reshaped(false, Some(bounds))
    }

    fn reshaped(&self, merge: bool, bounds: Option<Bounds>) -> Water {
        let clip = bounds.map(|b| tile::merc_extent(&b));
        Water { areas: merge::areas(&self.areas, merge, clip), lines: merge::lines(&self.lines, clip) }
    }

    /// Everything as a GeoJSON FeatureCollection in lon/lat, with a `class`
    /// property per feature: one MultiPolygon per area class (or per tile
    /// piece with `pieces`), one LineString per waterway piece.
    pub fn to_geojson(&self, opts: &GeoJsonOptions) -> String {
        geojson::write(&self.reshaped(!opts.pieces, opts.bounds))
    }
}

/// How [`Water::to_geojson`] shapes its output.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GeoJsonOptions {
    /// Keep areas as the pieces the tiles cut them into, instead of joined.
    pub pieces: bool,
    /// Cut everything to this box (west, south, east, north).
    pub bounds: Option<Bounds>,
}

/// A north-up pixel grid over a lon/lat box, in Web Mercator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub bounds: Bounds,
    pub width: usize,
    pub height: usize,
    /// Mercator extent: xmin, ymin, xmax, ymax in metres.
    pub merc: [f64; 4],
}

impl Grid {
    pub fn new(bounds: Bounds, width: usize, height: usize) -> Self {
        Grid { bounds, width: width.max(1), height: height.max(1), merc: tile::merc_extent(&bounds) }
    }

    /// Height follows from the box's shape in Mercator.
    pub fn with_width(bounds: Bounds, width: usize) -> Self {
        let m = tile::merc_extent(&bounds);
        let height = (width as f64 * (m[3] - m[1]) / (m[2] - m[0])).round() as usize;
        Self::new(bounds, width, height)
    }

    /// Metres per pixel in Mercator units (ground metres × 1/cos(latitude)).
    pub fn merc_px(&self) -> f64 {
        (self.merc[2] - self.merc[0]) / self.width as f64
    }

    /// The tile zoom with at least this grid's detail; see [`zoom_for`].
    pub fn zoom(&self, limits: &ZoomLimits) -> u8 {
        zoom_for(&self.bounds, self.width, limits)
    }

    pub fn merc_to_px(&self, p: [f64; 2]) -> [f32; 2] {
        let [x0, y0, x1, y1] = self.merc;
        [((p[0] - x0) / (x1 - x0) * self.width as f64) as f32, ((y1 - p[1]) / (y1 - y0) * self.height as f64) as f32]
    }

    pub fn px_to_lonlat(&self, x: f64, y: f64) -> [f64; 2] {
        let [x0, y0, x1, y1] = self.merc;
        merc_to_lonlat(x0 + x / self.width as f64 * (x1 - x0), y1 - y / self.height as f64 * (y1 - y0))
    }
}

/// Bounds on the zoom [`zoom_for`] picks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomLimits {
    /// Deepest zoom the tile source has.
    pub max_zoom: u8,
    /// Zoom out until the area needs no more tiles than this.
    pub max_tiles: usize,
}

impl Default for ZoomLimits {
    fn default() -> Self {
        ZoomLimits { max_zoom: MAX_ZOOM, max_tiles: 256 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaskOptions {
    /// Sub-rows per pixel row; columns are exact. 4 gives smooth edges.
    pub supersample: u32,
    /// Burn waterway lines in at this width, in pixels. 0 leaves them out.
    pub line_width: f64,
}

impl Default for MaskOptions {
    fn default() -> Self {
        MaskOptions { supersample: 4, line_width: 0.0 }
    }
}

/// Fraction of each pixel covered by water, row 0 at the top.
#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub coverage: Vec<f32>,
}

impl Mask {
    /// The shoreline: where coverage crosses one half, as polylines in pixels
    /// with water on the left. Rings are closed (last point = first); lines
    /// that leave the grid are open.
    pub fn outlines(&self) -> Vec<Vec<[f32; 2]>> {
        contour::outlines(&self.coverage, self.width, self.height, 0.5)
    }

    /// Distance to the shore in pixels: positive in water, negative on land.
    /// Infinite where the grid has no shore at all.
    pub fn distance(&self) -> Vec<f32> {
        distance::signed(&self.coverage, self.width, self.height)
    }

    /// Share of the grid that is water.
    pub fn water_fraction(&self) -> f64 {
        self.coverage.iter().map(|&c| c as f64).sum::<f64>() / self.coverage.len().max(1) as f64
    }
}
