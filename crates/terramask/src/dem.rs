//! Elevation from terrain tiles: sampled onto grids, and vectorised into
//! bands that cut areas, the sea into depth zones, the land into heights.
//!
//! The areas keep their own edges (the shoreline stays OpenStreetMap's);
//! the terrain only decides where one band gives way to the next.

use std::collections::HashMap;
use std::sync::OnceLock;

use i_overlay::core::overlay_rule::OverlayRule;

use crate::merge::{overlay, paths, rect, Path};
use crate::{contour, lonlat_to_merc, Error, Features, Grid, TileId};

/// Terrain tiles on AWS Open Data: land heights and sea depths as Terrarium
/// PNGs, no key. Sources and attribution:
/// <https://github.com/tilezen/joerd/blob/master/docs/attribution.md>.
pub const TERRARIUM: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png";
/// Deepest zoom of [`TERRARIUM`].
pub const TERRARIUM_MAX_ZOOM: u8 = 15;
/// Deepest zoom read by default. From zoom 11 on, some coasts (much of the
/// US) come from land surveys that flatten the sea to 0 m; up to 10 the sea
/// floor is there everywhere, and it has no more detail deeper down. Land
/// heights do: go deeper for those, and [`crate::Fetcher`] puts the sea
/// floor back from zoom 10 (see [`Elevation::fill_sea`]).
pub const TERRARIUM_ZOOM: u8 = 10;

/// Below any level: the frame that closes every band at the edge of the tiles.
const LOW: f32 = -1.0e7;

/// Heights in metres from square terrain tiles of one zoom: land above sea
/// level, the sea floor below it.
#[derive(Debug, Clone, Default)]
pub struct Elevation {
    zoom: u8,
    size: usize,
    tiles: HashMap<(u32, u32), Vec<f32>>,
    cubic: bool,
    /// The tiles stitched into one grid on the first sample, so sampling
    /// costs no lookups; `None` where they are too scattered for that.
    mosaic: OnceLock<Option<Mosaic>>,
}

#[derive(Debug, Clone)]
struct Mosaic {
    /// World pixel of the grid's top-left corner.
    gx0: i64,
    gy0: i64,
    w: i64,
    h: i64,
    values: Vec<f32>,
}

impl Mosaic {
    fn value(&self, gx: i64, gy: i64) -> f32 {
        let (x, y) = (gx - self.gx0, gy - self.gy0);
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            f32::NAN
        } else {
            self.values[(y * self.w + x) as usize]
        }
    }
}

impl Elevation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sample with Catmull-Rom between pixel centres instead of bilinear:
    /// slopes without creases where the terrain is magnified past its
    /// pixels, which contours and lines drawn along the slope would follow.
    pub fn cubic(mut self, on: bool) -> Self {
        self.cubic = on;
        self
    }

    pub fn zoom(&self) -> u8 {
        self.zoom
    }

    /// The tiles holding sea at exactly 0 m: above zoom [`TERRARIUM_ZOOM`]
    /// Terrarium has whole tiles of open sea flattened to sea level, which
    /// [`Elevation::fill_sea`] can take back from a coarser zoom.
    pub fn flat_sea_tiles(&self) -> Vec<TileId> {
        let mut ids: Vec<TileId> =
            self.tiles.iter().filter(|(_, t)| t.contains(&0.0)).map(|(&(x, y), _)| TileId::new(self.zoom, x, y)).collect();
        ids.sort_by_key(|t| (t.y, t.x));
        ids
    }

    /// Pixels at exactly 0 m take `coarser`'s height where that is below sea
    /// level: the sea floor back under flattened sea. Land, and sea that is
    /// 0 m in `coarser` too, stay as they are.
    pub fn fill_sea(&mut self, coarser: &Elevation) {
        if coarser.is_empty() || self.is_empty() {
            return;
        }
        let (res, [ox, oy]) = self.frame();
        let s = self.size;
        for (&(tx, ty), t) in self.tiles.iter_mut() {
            for (i, v) in t.iter_mut().enumerate().filter(|(_, v)| **v == 0.0) {
                let (gx, gy) = ((tx as usize * s + i % s) as f64 + 0.5, (ty as usize * s + i / s) as f64 + 0.5);
                let e = coarser.sample([ox + gx * res, oy - gy * res]);
                if e < 0.0 {
                    *v = e;
                }
            }
        }
        self.mosaic = OnceLock::new();
    }

    /// Read a Terrarium PNG: metres = R × 256 + G + B / 256 − 32768.
    pub fn add_tile(&mut self, id: TileId, png: &[u8]) -> Result<(), Error> {
        let (size, heights) = terrarium(png).map_err(|e| Error::Data(format!("elevation tile {id}: {e}")))?;
        self.add_heights(id, size, heights)
    }

    /// Heights already decoded: `size` × `size` metres, row by row from the
    /// north-west. Every tile must share one zoom and size.
    pub fn add_heights(&mut self, id: TileId, size: usize, heights: Vec<f32>) -> Result<(), Error> {
        if size == 0 || heights.len() != size * size {
            return Err(Error::Input(format!("elevation tile {id}: {} heights for {size}×{size}", heights.len())));
        }
        if !self.tiles.is_empty() && (id.z != self.zoom || size != self.size) {
            return Err(Error::Input(format!(
                "elevation tile {id}: zoom {} and {size} px, the others are zoom {} and {} px",
                id.z, self.zoom, self.size
            )));
        }
        self.zoom = id.z;
        self.size = size;
        self.tiles.insert((id.x, id.y), heights);
        self.mosaic = OnceLock::new();
        Ok(())
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Mercator metres per pixel, and the world's north-west corner.
    fn frame(&self) -> (f64, [f64; 2]) {
        let world = TileId::new(self.zoom, 0, 0);
        let [x0, _, _, y1] = world.merc_bounds();
        (world.merc_size() / self.size as f64, [x0, y1])
    }

    /// A pixel of the whole world at this zoom; NaN where there is no tile.
    fn value(&self, gx: i64, gy: i64) -> f32 {
        match self.mosaic.get_or_init(|| self.stitch()) {
            Some(m) => m.value(gx, gy),
            None => self.tile_value(gx, gy),
        }
    }

    /// The tiles in one grid over their bounding rectangle, NaN where one is
    /// missing; `None` when that rectangle would be mostly holes.
    fn stitch(&self) -> Option<Mosaic> {
        let s = self.size as i64;
        let xs = self.tiles.keys().map(|k| k.0 as i64);
        let ys = self.tiles.keys().map(|k| k.1 as i64);
        let (tx0, tx1) = (xs.clone().min()?, xs.max()?);
        let (ty0, ty1) = (ys.clone().min()?, ys.max()?);
        let (nx, ny) = (tx1 - tx0 + 1, ty1 - ty0 + 1);
        if (nx * ny) as usize > 4 * self.tiles.len() {
            return None;
        }
        let (w, h) = (nx * s, ny * s);
        let mut values = vec![f32::NAN; (w * h) as usize];
        for (&(tx, ty), t) in &self.tiles {
            let (ox, oy) = ((tx as i64 - tx0) * s, (ty as i64 - ty0) * s);
            for (r, row) in t.chunks_exact(s as usize).enumerate() {
                let at = ((oy + r as i64) * w + ox) as usize;
                values[at..at + s as usize].copy_from_slice(row);
            }
        }
        Some(Mosaic { gx0: tx0 * s, gy0: ty0 * s, w, h, values })
    }

    fn tile_value(&self, gx: i64, gy: i64) -> f32 {
        let s = self.size as i64;
        if gx < 0 || gy < 0 || s == 0 {
            return f32::NAN;
        }
        match self.tiles.get(&((gx / s) as u32, (gy / s) as u32)) {
            Some(t) => t[((gy % s) * s + gx % s) as usize],
            None => f32::NAN,
        }
    }

    /// Bilinear (or Catmull-Rom, see [`Elevation::cubic`]) between pixel
    /// centres; NaN outside the tiles.
    fn sample(&self, p: [f64; 2]) -> f32 {
        if self.tiles.is_empty() {
            return f32::NAN;
        }
        let (res, [ox, oy]) = self.frame();
        let (fx, fy) = ((p[0] - ox) / res - 0.5, (oy - p[1]) / res - 0.5);
        let (x, y) = (fx.floor(), fy.floor());
        let (tx, ty) = ((fx - x) as f32, (fy - y) as f32);
        let (x, y) = (x as i64, y as i64);
        if self.cubic {
            let (wx, wy) = (catmull_rom(tx), catmull_rom(ty));
            let mut sum = 0.0;
            for (j, wy) in wy.iter().enumerate() {
                for (i, wx) in wx.iter().enumerate() {
                    sum += wx * wy * self.value(x - 1 + i as i64, y - 1 + j as i64);
                }
            }
            if sum.is_finite() {
                return sum;
            }
        }
        let [a, b, c, d] = [self.value(x, y), self.value(x + 1, y), self.value(x, y + 1), self.value(x + 1, y + 1)];
        if [a, b, c, d].iter().all(|v| v.is_finite()) {
            (a * (1.0 - tx) + b * tx) * (1.0 - ty) + (c * (1.0 - tx) + d * tx) * ty
        } else {
            self.value(fx.round() as i64, fy.round() as i64)
        }
    }

    /// Metres at a point; NaN outside the tiles.
    pub fn at(&self, lon: f64, lat: f64) -> f32 {
        let [x, y] = lonlat_to_merc(lon, lat);
        self.sample([x, y])
    }

    /// Metres at each pixel centre of the grid, row 0 at the top; NaN
    /// outside the tiles.
    pub fn on(&self, grid: &Grid) -> Vec<f32> {
        let [x0, y0, x1, y1] = grid.merc;
        let (w, h) = (grid.width, grid.height);
        let mut out = Vec::with_capacity(w * h);
        for j in 0..h {
            let y = y1 - (j as f64 + 0.5) / h as f64 * (y1 - y0);
            out.extend((0..w).map(|i| self.sample([x0 + (i as f64 + 0.5) / w as f64 * (x1 - x0), y])));
        }
        out
    }

    /// The tiles as one grid, framed by two pixels: the edge repeated, then
    /// [`LOW`], so every contour closes just outside the tiles.
    fn field(&self) -> Field {
        let s = self.size as i64;
        let xs = self.tiles.keys().map(|k| k.0 as i64);
        let ys = self.tiles.keys().map(|k| k.1 as i64);
        let (tx0, tx1) = (xs.clone().min().unwrap_or(0), xs.max().unwrap_or(0));
        let (ty0, ty1) = (ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0));
        let (w, h) = (((tx1 - tx0 + 1) * s + 4) as usize, ((ty1 - ty0 + 1) * s + 4) as usize);
        let (gx0, gy0) = (tx0 * s - 2, ty0 * s - 2);
        let mut values = Vec::with_capacity(w * h);
        for j in 0..h {
            for i in 0..w {
                values.push(if i == 0 || j == 0 || i == w - 1 || j == h - 1 {
                    LOW
                } else {
                    let v = self.value(gx0 + i.clamp(2, w - 3) as i64, gy0 + j.clamp(2, h - 3) as i64);
                    if v.is_finite() {
                        v
                    } else {
                        LOW
                    }
                });
            }
        }
        let (res, [ox, oy]) = self.frame();
        let extent = [
            ox + (tx0 * s) as f64 * res,
            oy - ((ty1 + 1) * s) as f64 * res,
            ox + ((tx1 + 1) * s) as f64 * res,
            oy - (ty0 * s) as f64 * res,
        ];
        Field { values, w, h, origin: [ox + gx0 as f64 * res, oy - gy0 as f64 * res], res, extent }
    }
}

struct Field {
    values: Vec<f32>,
    w: usize,
    h: usize,
    /// Mercator position of the field's top-left corner.
    origin: [f64; 2],
    res: f64,
    /// Mercator extent of the tiles themselves.
    extent: [f64; 4],
}

impl Field {
    /// Where the terrain is at or above `level`, as closed rings in Mercator
    /// wound for the nonzero rule (the contours keep the high side on one hand).
    fn above(&self, level: f32) -> Vec<Path> {
        let [ox, oy] = self.origin;
        contour::outlines(&self.values, self.w, self.h, level)
            .into_iter()
            .filter(|l| l.len() >= 4)
            .map(|l| l[..l.len() - 1].iter().map(|p| [ox + p[0] as f64 * self.res, oy - p[1] as f64 * self.res]).collect())
            .collect()
    }
}

/// Catmull-Rom weights of the four samples around fraction `t`.
fn catmull_rom(t: f32) -> [f32; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [0.5 * (-t3 + 2.0 * t2 - t), 0.5 * (3.0 * t3 - 5.0 * t2 + 2.0), 0.5 * (-3.0 * t3 + 4.0 * t2 + t), 0.5 * (t3 - t2)]
}

/// Size and heights of a square Terrarium PNG.
fn terrarium(png: &[u8]) -> Result<(usize, Vec<f32>), String> {
    let mut dec = png::Decoder::new(png);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        c => return Err(format!("{c:?} PNG, not RGB")),
    };
    if info.width != info.height {
        return Err(format!("{}×{} PNG, not square", info.width, info.height));
    }
    let heights = buf[..info.buffer_size()]
        .chunks_exact(channels)
        .map(|p| p[0] as f32 * 256.0 + p[1] as f32 + p[2] as f32 / 256.0 - 32768.0)
        .collect();
    Ok((info.width as usize, heights))
}

impl Features {
    /// Every area cut into elevation bands at `levels` (metres; depths are
    /// negative): below the lowest level, between each pair, above the
    /// highest. Each piece is an [`crate::Area`] with its band in
    /// `elevation`, joined across tiles like [`Features::merged`]. Areas
    /// beyond the elevation tiles are left out; lines are kept as they are.
    ///
    /// The terrain is a coarser and older picture than the areas: near a
    /// shore it can put sea above sea level. Those bits fall in the highest
    /// band, so the bands still cover the area exactly.
    pub fn split(&self, elevation: &Elevation, levels: &[f64]) -> Result<Features, Error> {
        if elevation.is_empty() {
            return Err(Error::Input("no elevation tiles".into()));
        }
        if let Some(l) = levels.iter().find(|l| !l.is_finite() || **l <= LOW as f64) {
            return Err(Error::Input(format!("level {l} m")));
        }
        let mut levels = levels.to_vec();
        levels.sort_by(f64::total_cmp);
        levels.dedup();
        let field = elevation.field();
        let above: Vec<Vec<Path>> = levels.iter().map(|&l| field.above(l as f32)).collect();
        let n = levels.len();
        // Between two levels: above the lower, not above the higher.
        let between: Vec<Vec<Path>> = (1..n).map(|k| paths(&overlay(&above[k - 1], &above[k], OverlayRule::Difference))).collect();
        let tiles = rect(field.extent);
        let mut areas = Vec::new();
        for group in self.merged(None).areas {
            let s = paths(&overlay(&paths(&group.rings), &tiles, OverlayRule::Intersect));
            if s.is_empty() {
                continue;
            }
            for k in 0..=n {
                let rings = match k {
                    0 if n == 0 => overlay(&s, &[], OverlayRule::Subject),
                    0 => overlay(&s, &above[0], OverlayRule::Difference),
                    k if k == n => overlay(&s, &above[n - 1], OverlayRule::Intersect),
                    k => overlay(&s, &between[k - 1], OverlayRule::Intersect),
                };
                if !rings.is_empty() {
                    let lo = if k == 0 { f64::NEG_INFINITY } else { levels[k - 1] };
                    let hi = if k == n { f64::INFINITY } else { levels[k] };
                    let mut a = group.without_rings();
                    a.rings = rings;
                    a.elevation = Some([lo, hi]);
                    areas.push(a);
                }
            }
        }
        Ok(Features { areas, lines: self.lines.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cone 100 m high at the centre of one tile, 0 m at its edge.
    fn cone() -> Elevation {
        let s = 64;
        let h = (0..s * s)
            .map(|i| {
                let (x, y) = ((i % s) as f32 + 0.5 - 32.0, (i / s) as f32 + 0.5 - 32.0);
                (100.0 - x.hypot(y) * 100.0 / 32.0).max(0.0)
            })
            .collect();
        let mut e = Elevation::new();
        e.add_heights(TileId::new(10, 300, 400), s, h).unwrap();
        e
    }

    #[test]
    fn bands_are_rings_around_the_peak_and_cover_the_area() {
        let e = cone();
        let b = TileId::new(10, 300, 400).merc_bounds();
        let all = Features {
            areas: vec![crate::Area::new("land", "land", vec![crate::Ring { exterior: true, points: rect(b).remove(0) }])],
            lines: vec![],
        };
        let bands = all.split(&e, &[50.0, 25.0, 75.0]).unwrap();
        let got: Vec<[f64; 2]> = bands.areas.iter().map(|a| a.elevation.unwrap()).collect();
        assert_eq!(got, [[f64::NEG_INFINITY, 25.0], [25.0, 50.0], [50.0, 75.0], [75.0, f64::INFINITY]]);
        let area = |f: &Features| f.areas.iter().flat_map(|a| &a.rings).map(|r| crate::merge::signed_area(&r.points)).sum::<f64>();
        let whole = (b[2] - b[0]) * (b[3] - b[1]);
        assert!((area(&bands) - whole).abs() < whole * 1e-6, "{} vs {whole}", area(&bands));
        // The top band is a disc of radius 32 × 25/100 pixels.
        let px = (b[2] - b[0]) / 64.0;
        let top = area(&bands.within(75.0, f64::INFINITY));
        let disc = std::f64::consts::PI * (8.0 * px).powi(2);
        assert!((top - disc).abs() < disc * 0.05, "{top} vs {disc}");
        assert!(e.at(0.0, 0.0).is_nan());
    }

    #[test]
    fn heights_follow_the_grid() {
        let e = cone();
        let [w, s] = crate::merc_to_lonlat(TileId::new(10, 300, 400).merc_bounds()[0], TileId::new(10, 300, 400).merc_bounds()[1]);
        let [east, n] = crate::merc_to_lonlat(TileId::new(10, 300, 400).merc_bounds()[2], TileId::new(10, 300, 400).merc_bounds()[3]);
        let g = Grid::new([w, s, east, n], 64, 64);
        let h = e.on(&g);
        assert!((h[32 * 64 + 32] - 100.0).abs() < 5.0 && h[0] == 0.0);
        assert!(Elevation::new().add_heights(TileId::new(10, 0, 0), 4, vec![0.0; 3]).is_err());
        let mut mixed = cone();
        assert!(mixed.add_heights(TileId::new(11, 0, 0), 64, vec![0.0; 64 * 64]).is_err());
    }

    #[test]
    fn tiles_added_after_sampling_and_scattered_tiles_are_read() {
        let centre = |id: TileId| {
            let [x0, y0, x1, y1] = id.merc_bounds();
            crate::merc_to_lonlat((x0 + x1) / 2.0, (y0 + y1) / 2.0)
        };
        let (a, b, far) = (TileId::new(10, 300, 400), TileId::new(10, 301, 400), TileId::new(10, 900, 100));
        let mut e = Elevation::new();
        e.add_heights(a, 4, vec![1.0; 16]).unwrap();
        let [lon, lat] = centre(b);
        assert!(e.at(lon, lat).is_nan());
        e.add_heights(b, 4, vec![2.0; 16]).unwrap();
        assert_eq!(e.at(lon, lat), 2.0);
        e.add_heights(far, 4, vec![3.0; 16]).unwrap();
        let [lon, lat] = centre(far);
        assert_eq!(e.at(lon, lat), 3.0);
        assert!(e.stitch().is_none());
    }

    #[test]
    fn flattened_sea_takes_the_floor_from_the_coarser_zoom() {
        // Zoom 11: land on the west half, sea flattened to 0 m on the east.
        let s = 8;
        let fine = (0..s * s).map(|i| if i % s < s / 2 { 20.0 } else { 0.0 }).collect();
        let mut e = Elevation::new();
        let id = TileId::new(11, 600, 800);
        e.add_heights(id, s, fine).unwrap();
        assert_eq!(e.flat_sea_tiles(), [id]);
        assert_eq!(id.ancestor(10), TileId::new(10, 300, 400));
        let mut coarse = Elevation::new();
        coarse.add_heights(id.ancestor(10), s, vec![-40.0; s * s]).unwrap();
        e.fill_sea(&coarse);
        let [x0, y0, x1, y1] = id.merc_bounds();
        let at = |fx: f64| {
            let [lon, lat] = crate::merc_to_lonlat(x0 + fx * (x1 - x0), (y0 + y1) / 2.0);
            e.at(lon, lat)
        };
        assert_eq!((at(0.2), at(0.8)), (20.0, -40.0));
        assert!(e.flat_sea_tiles().is_empty());
    }

    #[test]
    fn cubic_keeps_ramps_and_smooths_corners() {
        let s = 16;
        let ramp: Vec<f32> = (0..s * s).map(|i| (i % s) as f32 * 10.0).collect();
        let id = TileId::new(10, 300, 400);
        let mut e = Elevation::new();
        e.add_heights(id, s, ramp).unwrap();
        let e = e.cubic(true);
        let [x0, y0, x1, y1] = id.merc_bounds();
        let (px, py) = ((x1 - x0) / s as f64, (y1 - y0) / s as f64);
        for fx in [5.1, 7.5, 9.9] {
            let [lon, lat] = crate::merc_to_lonlat(x0 + fx * px, y1 - 7.3 * py);
            assert!((e.at(lon, lat) - (fx as f32 - 0.5) * 10.0).abs() < 1e-3);
        }
        // At the edge, where there are no four neighbours, bilinear.
        let [lon, lat] = crate::merc_to_lonlat(x0 + 0.75 * px, y1 - 7.3 * py);
        assert!((e.at(lon, lat) - 2.5).abs() < 1e-3);
        // Over the cone's peak the cubic bends less sharply than bilinear.
        let (lin, cub) = (cone(), cone().cubic(true));
        let rough = |e: &Elevation| {
            let [x0, y0, x1, y1] = TileId::new(10, 300, 400).merc_bounds();
            let h: Vec<f32> = (0..400)
                .map(|i| {
                    let [lon, lat] = crate::merc_to_lonlat(x0 + (x1 - x0) * (0.3 + i as f64 / 1000.0), (y0 + y1) / 2.0);
                    e.at(lon, lat)
                })
                .collect();
            h.windows(3).map(|w| (w[0] - 2.0 * w[1] + w[2]).abs()).fold(0.0, f32::max)
        };
        assert!(rough(&cub) < rough(&lin), "{} vs {}", rough(&cub), rough(&lin));
    }
}
