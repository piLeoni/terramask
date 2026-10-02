//! Web Mercator tile arithmetic.

use std::f64::consts::PI;
use std::fmt;

use crate::ZoomLimits;

/// West, south, east, north in degrees. East may be less than west when the
/// box crosses the 180° meridian.
pub type Bounds = [f64; 4];

/// Deepest zoom of OpenMapTiles-schema tiles.
pub const MAX_ZOOM: u8 = 14;

const R: f64 = 6378137.0;
const WORLD: f64 = 2.0 * PI * R;
const MAX_LAT: f64 = 85.051_128_779_806_59;
/// Tiles are drawn 256 px wide; sources simplify for that size.
const TILE_PX: f64 = 256.0;

pub fn lonlat_to_merc(lon: f64, lat: f64) -> [f64; 2] {
    let lat = lat.clamp(-MAX_LAT, MAX_LAT);
    [R * lon.to_radians(), R * (PI / 4.0 + lat.to_radians() / 2.0).tan().ln()]
}

pub fn merc_to_lonlat(x: f64, y: f64) -> [f64; 2] {
    [(x / R).to_degrees(), (2.0 * (y / R).exp().atan() - PI / 2.0).to_degrees()]
}

/// Mercator extent of a lon/lat box, with east unwrapped past west.
pub(crate) fn merc_extent(b: &Bounds) -> [f64; 4] {
    let east = if b[2] < b[0] { b[2] + 360.0 } else { b[2] };
    let [x0, y0] = lonlat_to_merc(b[0], b[1]);
    let [x1, y1] = lonlat_to_merc(east, b[3]);
    [x0, y0.min(y1), x1, y0.max(y1)]
}

/// A tile. `x` may run past `2^z - 1` when an area crosses the 180°
/// meridian; [`TileId::wrapped_x`] is the tile to request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    pub fn new(z: u8, x: u32, y: u32) -> Self {
        TileId { z, x, y }
    }

    pub fn wrapped_x(&self) -> u32 {
        self.x % (1u32 << self.z)
    }

    /// Side in Mercator metres.
    pub fn merc_size(&self) -> f64 {
        WORLD / (1u64 << self.z) as f64
    }

    /// xmin, ymin, xmax, ymax in Mercator metres.
    pub fn merc_bounds(&self) -> [f64; 4] {
        let s = self.merc_size();
        let x0 = -WORLD / 2.0 + self.x as f64 * s;
        let y1 = WORLD / 2.0 - self.y as f64 * s;
        [x0, y1 - s, x0 + s, y1]
    }

    /// The tile at zoom `z` (at most this one's) that holds this one.
    pub fn ancestor(&self, z: u8) -> TileId {
        let up = self.z.saturating_sub(z);
        TileId { z: self.z - up, x: self.x >> up, y: self.y >> up }
    }
}

impl fmt::Display for TileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.z, self.wrapped_x(), self.y)
    }
}

fn tile_range(b: &Bounds, z: u8) -> (u32, u32, u32, u32) {
    let m = merc_extent(b);
    let n = (1u64 << z) as f64;
    let s = WORLD / n;
    let col = |x: f64| ((x + WORLD / 2.0) / s).floor().max(0.0) as u32;
    let row = |y: f64| ((WORLD / 2.0 - y) / s).floor().clamp(0.0, n - 1.0) as u32;
    // A box ending exactly on a tile edge doesn't need the next tile.
    let x1 = col(m[2] - 1e-6).max(col(m[0]));
    (col(m[0]), row(m[3]), x1, row(m[1] + 1e-6).max(row(m[3])))
}

/// Tiles covering the box at zoom `z`, row by row from the north-west.
pub fn tiles_for(b: &Bounds, z: u8) -> Vec<TileId> {
    let (x0, y0, x1, y1) = tile_range(b, z);
    (y0..=y1).flat_map(|y| (x0..=x1).map(move |x| TileId { z, x, y })).collect()
}

/// The shallowest zoom whose tiles hold at least the detail of `width`
/// pixels across the box, capped at `limits.max_zoom`, then lowered until the
/// box needs at most `limits.max_tiles` tiles.
pub fn zoom_for(b: &Bounds, width: usize, limits: &ZoomLimits) -> u8 {
    let m = merc_extent(b);
    let px = (m[2] - m[0]) / width.max(1) as f64;
    let want = (WORLD / (TILE_PX * px)).log2().ceil();
    let mut z = if want.is_finite() { want.clamp(0.0, limits.max_zoom as f64) as u8 } else { 0 };
    while z > 0 {
        let (x0, y0, x1, y1) = tile_range(b, z);
        if ((x1 - x0 + 1) as usize) * ((y1 - y0 + 1) as usize) <= limits.max_tiles.max(1) {
            break;
        }
        z -= 1;
    }
    z
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mercator_round_trip() {
        let [x, y] = lonlat_to_merc(7.74, 46.02);
        let [lon, lat] = merc_to_lonlat(x, y);
        assert!((lon - 7.74).abs() < 1e-9 && (lat - 46.02).abs() < 1e-9);
    }

    #[test]
    fn tile_bounds_match_the_scheme() {
        // Amsterdam at z12 is tile 2103/1346.
        let t = tiles_for(&[4.9, 52.37, 4.9001, 52.3701], 12);
        assert_eq!(t, vec![TileId::new(12, 2103, 1346)]);
        let b = t[0].merc_bounds();
        let [x, y] = lonlat_to_merc(4.9, 52.37);
        assert!(b[0] <= x && x <= b[2] && b[1] <= y && y <= b[3]);
    }

    #[test]
    fn zoom_follows_resolution_and_tile_budget() {
        let b = [-70.85, 41.3, -70.45, 41.55];
        let lim = ZoomLimits::default();
        // 0.4° is 44.5 km in Mercator: 1200 px of 37 m wants 256-px tiles of
        // ≤ 37 m pixels, first reached at z13 (z12 is 38 m).
        assert_eq!(zoom_for(&b, 1200, &lim), 13);
        assert_eq!(zoom_for(&b, 100_000, &ZoomLimits { max_tiles: 10_000, ..lim }), MAX_ZOOM);
        // z14 would take ~19 × 16 tiles, over the default budget of 256.
        assert_eq!(zoom_for(&b, 100_000, &lim), 13);
        assert!(zoom_for(&b, 100_000, &ZoomLimits { max_tiles: 4, ..lim }) < 12);
    }

    #[test]
    fn crossing_the_antimeridian() {
        let t = tiles_for(&[179.9, -16.9, -179.9, -16.8], 10);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].wrapped_x(), 1023);
        assert_eq!(t[1].wrapped_x(), 0);
        assert_eq!(t[1].x, 1024);
    }
}
