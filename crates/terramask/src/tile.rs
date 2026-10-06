//! Web Mercator tile arithmetic.

use std::f64::consts::PI;
use std::fmt;

use crate::ZoomLimits;

/// West, south, east, north in degrees. East may be less than west when the
/// box crosses the 180° meridian.
pub type Bounds = [f64; 4];

/// Deepest zoom of OpenMapTiles-schema tiles.
pub const MAX_ZOOM: u8 = 14;

pub(crate) const R: f64 = 6378137.0;
pub(crate) const WORLD: f64 = 2.0 * PI * R;
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

/// Tiles at zoom `z` that touch the polygon `ring` (lon/lat, closed or not),
/// row by row from the north-west: for a turned or thin area, far fewer than
/// [`tiles_for`] its bounding box. A ring spanning more than 180° of
/// longitude falls back to the box.
pub fn tiles_touching(ring: &[[f64; 2]], z: u8) -> Vec<TileId> {
    if ring.len() < 3 {
        return Vec::new();
    }
    let (w, e) = ring.iter().fold((f64::MAX, f64::MIN), |(w, e), p| (w.min(p[0]), e.max(p[0])));
    let (s, n) = ring.iter().fold((f64::MAX, f64::MIN), |(s, n), p| (s.min(p[1]), n.max(p[1])));
    let bounds = [w, s, e, n];
    if e - w > 180.0 {
        return tiles_for(&bounds, z);
    }
    let poly: Vec<[f64; 2]> = ring.iter().map(|p| lonlat_to_merc(p[0], p[1])).collect();
    tiles_for(&bounds, z).into_iter().filter(|t| touches(&poly, t.merc_bounds())).collect()
}

/// Whether the polygon `poly` (Mercator) and the rectangle `r` share any
/// point: an edge crossing it, a corner of `r` inside `poly`, or `poly`
/// inside `r`.
fn touches(poly: &[[f64; 2]], r: [f64; 4]) -> bool {
    let inside_r = |p: &[f64; 2]| p[0] >= r[0] && p[0] <= r[2] && p[1] >= r[1] && p[1] <= r[3];
    if poly.iter().any(inside_r) {
        return true;
    }
    let n = poly.len();
    if (0..n).any(|i| segment_hits_rect(poly[i], poly[(i + 1) % n], r)) {
        return true;
    }
    contains(poly, [(r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0])
}

/// Liang–Barsky: whether the segment `a → b` meets the rectangle.
fn segment_hits_rect(a: [f64; 2], b: [f64; 2], r: [f64; 4]) -> bool {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for (p, q) in [(-dx, a[0] - r[0]), (dx, r[2] - a[0]), (-dy, a[1] - r[1]), (dy, r[3] - a[1])] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

/// Even–odd point in polygon.
fn contains(poly: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
    }
    inside
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

/// Ground metres across this many MVT extent units at zoom `z` (a tile is 4096 units wide).
pub fn mvt_units_to_m(z: u8, units: f64) -> f64 {
    let tile_w = WORLD / (1u64 << z) as f64;
    tile_w / 4096.0 * units
}

/// Approximate degrees at latitude `lat` for `units` MVT extent units at zoom `z`.
pub fn mvt_units_to_deg(lat: f64, z: u8, units: f64) -> f64 {
    let m = mvt_units_to_m(z, units);
    m / (111_320.0 * lat.to_radians().cos().max(1e-6))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mvt_margin_scales_with_zoom() {
        let z12 = mvt_units_to_m(12, 64.0);
        let z14 = mvt_units_to_m(14, 64.0);
        assert!(z14 < z12 && z12 > 0.0);
    }

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

    /// A long thin strip turned 34°: its box is near square, the strip is not.
    fn turned_strip() -> Vec<[f64; 2]> {
        let (cx, cy, half_len, half_w, a) = (9.17, 45.47, 0.32, 0.0065, (-34.0_f64).to_radians());
        [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
            .iter()
            .map(|(u, v)| {
                let (x, y) = (u * half_len, v * half_w);
                [cx + x * a.cos() - y * a.sin(), cy + (x * a.sin() + y * a.cos()) * 0.7]
            })
            .collect()
    }

    #[test]
    fn a_turned_strip_needs_only_the_tiles_it_crosses() {
        let ring = turned_strip();
        let all = {
            let (w, e) = ring.iter().fold((f64::MAX, f64::MIN), |(w, e), p| (w.min(p[0]), e.max(p[0])));
            let (s, n) = ring.iter().fold((f64::MAX, f64::MIN), |(s, n), p| (s.min(p[1]), n.max(p[1])));
            tiles_for(&[w, s, e, n], 14)
        };
        let touched = tiles_touching(&ring, 14);
        assert!(touched.len() * 4 < all.len(), "{} of {}", touched.len(), all.len());
        // Every tile under a point of the strip is kept.
        for k in 0..=100 {
            let t = k as f64 / 100.0;
            let p = [ring[0][0] + (ring[2][0] - ring[0][0]) * t, ring[0][1] + (ring[2][1] - ring[0][1]) * t];
            let under = tiles_for(&[p[0], p[1], p[0], p[1]], 14)[0];
            assert!(touched.contains(&under), "{under} at {p:?}");
        }
    }

    #[test]
    fn a_small_area_inside_one_tile_keeps_it() {
        let ring = [[4.9, 52.37], [4.9001, 52.37], [4.9001, 52.3701], [4.9, 52.3701]];
        assert_eq!(tiles_touching(&ring, 12), vec![TileId::new(12, 2103, 1346)]);
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
