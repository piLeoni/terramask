//! Masks, outlines, polygons and distance grids of the world's water, land,
//! forests, glaciers, parks or any layer of OpenStreetMap vector tiles, for
//! any area, from tiles fetched on demand; depth and height bands from terrain
//! tiles.
//!
//! Areas and lines come from OpenMapTiles-schema vector tiles (OpenFreeMap by
//! default), picked by a [`Filter`]: the sea, lakes and rivers by default,
//! presets such as `land`, `forest` or `parks`, or any layer and class. Only
//! the tiles covering the area are read, at the zoom that matches the output
//! resolution.
//!
//! ```no_run
//! # #[cfg(feature = "fetch")] {
//! use terramask::{Fetcher, Filter, Grid, MaskOptions, ZoomLimits};
//! let bounds = [-70.85, 41.3, -70.45, 41.55]; // west, south, east, north
//! let grid = Grid::with_width(bounds, 1200);
//! let water = Fetcher::new().features_for(&grid, &Filter::default(), &ZoomLimits::default(), |_, _| {})?;
//! let mask = water.mask(&grid, &MaskOptions::default());
//! let shore = mask.outlines();          // polylines in pixels
//! let dist = mask.distance();           // pixels to the shore, + inside
//! let woods = Fetcher::new().features_for(&grid, &Filter::parse(&["forest", "parks"])?, &ZoomLimits::default(), |_, _| {})?;
//! # }
//! # Ok::<(), terramask::Error>(())
//! ```
//!
//! Nothing here needs the network: [`Features::add_tile`] takes tile bytes
//! from anywhere. The `fetch` feature adds [`Fetcher`], which downloads and
//! caches them. The `dem` feature adds [`Elevation`], from terrain tiles, to
//! cut areas into depth or height bands; those tiles are only fetched when
//! asked for.

mod contour;
#[cfg(feature = "dem")]
mod dem;
mod distance;
#[cfg(feature = "fetch")]
mod fetch;
mod geojson;
mod merge;
mod mvt;
mod raster;
mod select;
mod tile;

use std::fmt;

use i_overlay::core::overlay_rule::OverlayRule;

#[cfg(feature = "dem")]
pub use dem::{Elevation, TERRARIUM, TERRARIUM_MAX_ZOOM, TERRARIUM_ZOOM};
#[cfg(feature = "fetch")]
pub use fetch::{default_cache, Fetcher, Source, MAX_AGE, OPENFREEMAP};
pub use select::{Filter, Rule, LAND, PRESETS};
pub use tile::{
    lonlat_to_merc, merc_to_lonlat, mvt_units_to_deg, mvt_units_to_m, tiles_for, tiles_touching, zoom_for, Bounds, TileId, MAX_ZOOM,
};

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

/// A closed ring in Web Mercator metres. Exterior rings bound the area, the
/// others are holes in it (islands, for water).
#[derive(Debug, Clone, PartialEq)]
pub struct Ring {
    pub exterior: bool,
    pub points: Vec<[f64; 2]>,
}

/// One area, as cut by its tile.
#[derive(Debug, Clone, PartialEq)]
pub struct Area {
    /// Tile layer (`water`, `landcover`, `building`…), or [`LAND`].
    pub layer: String,
    /// Empty where the layer has none (`building`).
    pub class: String,
    /// The finer kind some layers give: `park` or `garden` under landcover
    /// `grass`, `footway` under transportation `path`. Empty when none.
    pub subclass: String,
    /// The feature's other attributes as text (`brunnel`, `render_height`,
    /// `name`…), sorted by key. Names in other languages are left out.
    pub tags: Vec<(String, String)>,
    pub rings: Vec<Ring>,
    /// Elevation band in metres, lowest and highest, open ends infinite:
    /// set by [`Features::split`].
    pub elevation: Option<[f64; 2]>,
    /// The tile that cut this piece out; `None` once joined or merged.
    pub tile: Option<TileId>,
}

impl Area {
    pub fn new(layer: &str, class: &str, rings: Vec<Ring>) -> Area {
        Area { layer: layer.into(), class: class.into(), subclass: String::new(), tags: Vec::new(), rings, elevation: None, tile: None }
    }

    /// The value of attribute `key`.
    pub fn tag(&self, key: &str) -> Option<&str> {
        tag(&self.tags, key)
    }

    fn without_rings(&self) -> Area {
        Area {
            layer: self.layer.clone(),
            class: self.class.clone(),
            subclass: self.subclass.clone(),
            tags: self.tags.clone(),
            rings: Vec::new(),
            elevation: self.elevation,
            tile: None,
        }
    }
}

/// One line (a waterway centre line, a road), as cut by its tile, in Web
/// Mercator metres.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub layer: String,
    pub class: String,
    /// As for [`Area::subclass`].
    pub subclass: String,
    /// As for [`Area::tags`].
    pub tags: Vec<(String, String)>,
    pub points: Vec<[f64; 2]>,
    /// The tile that cut this piece out; `None` once joined.
    pub tile: Option<TileId>,
}

impl Line {
    pub fn new(layer: &str, class: &str, points: Vec<[f64; 2]>) -> Line {
        Line { layer: layer.into(), class: class.into(), subclass: String::new(), tags: Vec::new(), points, tile: None }
    }

    /// The value of attribute `key`.
    pub fn tag(&self, key: &str) -> Option<&str> {
        tag(&self.tags, key)
    }

    fn without_points(&self) -> Line {
        Line {
            layer: self.layer.clone(),
            class: self.class.clone(),
            subclass: self.subclass.clone(),
            tags: self.tags.clone(),
            points: Vec::new(),
            tile: self.tile,
        }
    }
}

fn tag<'a>(tags: &'a [(String, String)], key: &str) -> Option<&'a str> {
    tags.binary_search_by(|(k, _)| k.as_str().cmp(key)).ok().map(|i| tags[i].1.as_str())
}

/// A point feature (`place`, `poi`…), in Web Mercator metres.
#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    pub layer: String,
    pub class: String,
    pub subclass: String,
    pub tags: Vec<(String, String)>,
    pub position: [f64; 2],
    pub tile: Option<TileId>,
}

impl Pin {
    pub fn tag(&self, key: &str) -> Option<&str> {
        tag(&self.tags, key)
    }
}

/// Areas and lines collected from tiles. Each tile's features are cut to the
/// tile, so neighbouring tiles meet edge to edge without overlapping; an area
/// spanning several tiles is several pieces.
#[derive(Debug, Clone, Default)]
pub struct Features {
    pub areas: Vec<Area>,
    pub lines: Vec<Line>,
    pub pins: Vec<Pin>,
}

impl Features {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one vector tile (raw or gzipped protobuf) and keep the features
    /// that pass `filter`. With [`LAND`] in the filter, an empty tile is all
    /// land: tile sources leave out tiles with nothing in them, and the sea
    /// is always something.
    pub fn add_tile(&mut self, id: TileId, bytes: &[u8], filter: &Filter) -> Result<(), Error> {
        let t = mvt::read(bytes, filter).map_err(|e| Error::Data(format!("tile {id}: {e}")))?;
        let [x0, _, _, y1] = id.merc_bounds();
        let size = id.merc_size();
        let to_merc = |pts: Vec<[f64; 2]>| pts.into_iter().map(|p| [x0 + p[0] * size, y1 - p[1] * size]).collect();
        let ring = |r: Ring| Ring { exterior: r.exterior, points: to_merc(r.points) };
        for a in t.areas {
            self.areas.push(Area { rings: a.rings.into_iter().map(ring).collect(), tile: Some(id), ..a });
        }
        for l in t.lines {
            self.lines.push(Line { points: to_merc(l.points), tile: Some(id), ..l });
        }
        for p in t.pins {
            let [x, y] = p.position;
            let [mx, my] = [x0 + x * size, y1 - y * size];
            self.pins.push(Pin { position: [mx, my], tile: Some(id), ..p });
        }
        if filter.land() {
            let sea: Vec<Ring> = t.sea.into_iter().map(ring).collect();
            let rings = merge::overlay(&merge::rect(id.merc_bounds()), &merge::paths(&sea), OverlayRule::Difference);
            if !rings.is_empty() {
                self.areas.push(Area { tile: Some(id), ..Area::new(LAND, LAND, rings) });
            }
        }
        Ok(())
    }

    /// The areas and lines whose layer and class (or subclass) pass `filter`
    /// (its `intermittent` and `tunnels` play no part here).
    pub fn subset(&self, filter: &Filter) -> Features {
        Features {
            areas: self.areas.iter().filter(|a| filter.keeps(&a.layer, &a.class, &a.subclass)).cloned().collect(),
            lines: self.lines.iter().filter(|l| filter.keeps(&l.layer, &l.class, &l.subclass)).cloned().collect(),
            pins: self.pins.iter().filter(|p| filter.keeps(&p.layer, &p.class, &p.subclass)).cloned().collect(),
        }
    }

    /// The areas whose elevation band lies within `low..=high` metres (see
    /// [`Features::split`]); lines are kept.
    pub fn within(&self, low: f64, high: f64) -> Features {
        let inside = |a: &&Area| a.elevation.is_some_and(|[lo, hi]| lo >= low && hi <= high);
        Features { areas: self.areas.iter().filter(inside).cloned().collect(), lines: self.lines.clone(), pins: self.pins.clone() }
    }

    /// Coverage on a north-up Web Mercator grid.
    pub fn mask(&self, grid: &Grid, opts: &MaskOptions) -> Mask {
        let [x0, y0, x1, y1] = grid.merc;
        let (sx, sy) = (grid.width as f64 / (x1 - x0), grid.height as f64 / (y1 - y0));
        self.rasterize(grid.width, grid.height, opts, &|p| [(p[0] - x0) * sx, (y1 - p[1]) * sy])
    }

    /// Coverage on any grid: `project` takes lon, lat in degrees and
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

    /// Lines on a Web Mercator grid, in pixels, in the order of
    /// [`Features::lines`].
    pub fn lines_on(&self, grid: &Grid) -> Vec<Vec<[f32; 2]>> {
        self.lines.iter().map(|l| l.points.iter().map(|&p| grid.merc_to_px(p)).collect()).collect()
    }

    /// The areas of each layer, class and elevation band joined across tile
    /// edges into one [`Area`], each exterior ring followed by its holes;
    /// with `bounds`, areas and lines cut to that box. Lines are not joined.
    /// The subclass and tags of a joined area are those its pieces share.
    pub fn merged(&self, bounds: Option<Bounds>) -> Features {
        self.reshaped(true, bounds)
    }

    /// Each feature whole again, and apart from its neighbours: the pieces
    /// tiles cut a building, a park or a road into are joined across the tile
    /// edges, while two buildings sharing a wall, or a park beside a garden,
    /// stay two areas. Areas keep their subclass and tags; one area per
    /// polygon (exterior ring and its holes). Lines with the same layer,
    /// class, subclass and tags whose cut ends meet on a tile edge become one
    /// line. With `bounds`, everything is cut to that box.
    pub fn joined(&self, bounds: Option<Bounds>) -> Features {
        let clip = bounds.map(|b| tile::merc_extent(&b));
        Features {
            areas: merge::join_areas(&self.areas, clip),
            lines: merge::join_lines(&self.lines, clip),
            pins: merge::clip_pins(&self.pins, clip),
        }
    }

    /// Areas and lines cut to `bounds`, the tile pieces kept apart.
    pub fn clipped(&self, bounds: Bounds) -> Features {
        self.reshaped(false, Some(bounds))
    }

    fn reshaped(&self, merge: bool, bounds: Option<Bounds>) -> Features {
        let clip = bounds.map(|b| tile::merc_extent(&b));
        Features {
            areas: merge::areas(&self.areas, merge, clip),
            lines: merge::lines(&self.lines, clip),
            pins: merge::clip_pins(&self.pins, clip),
        }
    }

    /// Everything as a GeoJSON FeatureCollection in lon/lat, with `layer`
    /// and `class` properties (and `min`, `max` metres for elevation bands):
    /// one MultiPolygon per layer, class and band (or per tile piece with
    /// `pieces`), one LineString per line piece.
    pub fn to_geojson(&self, opts: &GeoJsonOptions) -> String {
        geojson::write(&self.reshaped(!opts.pieces, opts.bounds))
    }
}

/// How [`Features::to_geojson`] shapes its output.
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

/// Fraction of each pixel covered by the areas, row 0 at the top. Below,
/// "water" is whatever the areas are: forest, land, a depth band.
#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub coverage: Vec<f32>,
}

impl Mask {
    /// Coverage of rings already in pixels (x right, y down), filled as
    /// [`Features::mask`] fills areas: exteriors union, holes cut, edge
    /// pixels partly covered; `supersample` sub-rows per pixel row. For
    /// polygons from anywhere, not only tiles.
    pub fn from_rings(width: usize, height: usize, rings: &[Ring], supersample: u32) -> Mask {
        let mut r = raster::Raster::new(width, height);
        for ring in rings {
            r.add_ring(ring.points.clone(), ring.exterior);
        }
        Mask { width, height, coverage: r.fill(supersample.max(1)) }
    }

    /// The shoreline: where coverage crosses one half, as polylines in pixels
    /// with water on the left. Rings are closed (last point = first); lines
    /// that leave the grid are open.
    pub fn outlines(&self) -> Vec<Vec<[f32; 2]>> {
        contour::outlines(&self.coverage, self.width, self.height, 0.5)
    }

    /// Distance to the edge in pixels: positive inside, negative outside.
    /// Infinite where the grid has no shore at all.
    pub fn distance(&self) -> Vec<f32> {
        distance::signed(&self.coverage, self.width, self.height)
    }

    /// Share of the grid that is covered.
    pub fn fraction(&self) -> f64 {
        self.coverage.iter().map(|&c| c as f64).sum::<f64>() / self.coverage.len().max(1) as f64
    }
}
