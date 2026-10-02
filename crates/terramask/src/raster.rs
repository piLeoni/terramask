//! Polygon coverage by scanlines: nonzero winding (so pieces from different
//! tiles union and islands cut holes), several sub-rows per pixel row and
//! exact span coverage along each sub-row.

struct Edge {
    /// Top end (smaller y) first.
    x0: f64,
    y0: f64,
    y1: f64,
    /// dx per unit y.
    slope: f64,
    winding: i32,
}

pub struct Raster {
    width: usize,
    height: usize,
    edges: Vec<Edge>,
}

fn signed_area(pts: &[[f64; 2]]) -> f64 {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        / 2.0
}

impl Raster {
    pub fn new(width: usize, height: usize) -> Self {
        Raster { width, height, edges: Vec::new() }
    }

    /// A closed ring in pixels. Exteriors wind one way and holes the other
    /// whatever the projection did to them, so the winding rule holds.
    pub fn add_ring(&mut self, mut pts: Vec<[f64; 2]>, exterior: bool) {
        if pts.len() < 3 || pts.iter().any(|p| !p[0].is_finite() || !p[1].is_finite()) {
            return;
        }
        if (signed_area(&pts) > 0.0) != exterior {
            pts.reverse();
        }
        let n = pts.len();
        for i in 0..n {
            self.add_edge(pts[i], pts[(i + 1) % n]);
        }
    }

    /// A polyline `width` pixels wide: a quad per segment, an octagon per joint.
    pub fn add_stroke(&mut self, pts: &[[f64; 2]], width: f64) {
        let r = width / 2.0;
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len = dx.hypot(dy);
            if len == 0.0 {
                continue;
            }
            let (nx, ny) = (-dy / len * r, dx / len * r);
            self.add_ring(vec![[a[0] + nx, a[1] + ny], [b[0] + nx, b[1] + ny], [b[0] - nx, b[1] - ny], [a[0] - nx, a[1] - ny]], true);
        }
        for p in pts.iter().skip(1).take(pts.len().saturating_sub(2)) {
            let oct = (0..8)
                .map(|k| {
                    let t = k as f64 * std::f64::consts::FRAC_PI_4;
                    [p[0] + r * t.cos(), p[1] + r * t.sin()]
                })
                .collect();
            self.add_ring(oct, true);
        }
    }

    fn add_edge(&mut self, a: [f64; 2], b: [f64; 2]) {
        if a[1] == b[1] {
            return;
        }
        let (top, bot, winding) = if a[1] < b[1] { (a, b, 1) } else { (b, a, -1) };
        if bot[1] <= 0.0 || top[1] >= self.height as f64 {
            return;
        }
        self.edges.push(Edge { x0: top[0], y0: top[1], y1: bot[1], slope: (bot[0] - top[0]) / (bot[1] - top[1]), winding });
    }

    /// Coverage in 0..1 per pixel, row 0 at the top.
    pub fn fill(mut self, supersample: u32) -> Vec<f32> {
        let (w, h) = (self.width, self.height);
        let mut out = vec![0f32; w * h];
        self.edges.sort_by(|a, b| a.y0.total_cmp(&b.y0));
        let ss = supersample as usize;
        let weight = 1.0 / ss as f64;
        let mut next = 0;
        let mut active: Vec<usize> = Vec::new();
        let mut xs: Vec<(f64, i32)> = Vec::new();
        let mut row = vec![0f64; w + 1];
        for py in 0..h {
            row.iter_mut().for_each(|v| *v = 0.0);
            for k in 0..ss {
                let y = py as f64 + (k as f64 + 0.5) * weight;
                while next < self.edges.len() && self.edges[next].y0 <= y {
                    active.push(next);
                    next += 1;
                }
                active.retain(|&i| self.edges[i].y1 > y);
                xs.clear();
                for &i in &active {
                    let e = &self.edges[i];
                    if e.y0 <= y {
                        xs.push((e.x0 + (y - e.y0) * e.slope, e.winding));
                    }
                }
                xs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut wind = 0;
                let mut start = 0.0;
                for &(x, d) in &xs {
                    let was = wind != 0;
                    wind += d;
                    if !was && wind != 0 {
                        start = x;
                    } else if was && wind == 0 {
                        span(&mut row, w, start, x, weight);
                    }
                }
            }
            for (o, v) in out[py * w..(py + 1) * w].iter_mut().zip(&row) {
                *o = v.clamp(0.0, 1.0) as f32;
            }
        }
        out
    }
}

/// Add `weight` × the covered length of [a, b) to each pixel of the row.
fn span(row: &mut [f64], w: usize, a: f64, b: f64, weight: f64) {
    let (a, b) = (a.max(0.0), b.min(w as f64));
    if b <= a {
        return;
    }
    let (ia, ib) = (a.floor() as usize, b.floor() as usize);
    if ia == ib {
        row[ia] += (b - a) * weight;
        return;
    }
    row[ia] += (ia as f64 + 1.0 - a) * weight;
    for v in &mut row[ia + 1..ib] {
        *v += weight;
    }
    if ib < w {
        row[ib] += (b - ib as f64) * weight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(v: &[f32]) -> f64 {
        v.iter().map(|&c| c as f64).sum()
    }

    #[test]
    fn square_with_a_hole() {
        let mut r = Raster::new(20, 20);
        r.add_ring(vec![[2.0, 2.0], [18.0, 2.0], [18.0, 18.0], [2.0, 18.0]], true);
        r.add_ring(vec![[6.0, 6.0], [6.0, 14.0], [14.0, 14.0], [14.0, 6.0]], false);
        let c = r.fill(4);
        assert!((sum(&c) - (256.0 - 64.0)).abs() < 1e-3);
        assert_eq!(c[10 * 20 + 10], 0.0);
        assert_eq!(c[3 * 20 + 3], 1.0);
    }

    #[test]
    fn pieces_meeting_edge_to_edge_union_without_seam() {
        let mut r = Raster::new(10, 4);
        r.add_ring(vec![[0.0, 0.0], [4.3, 0.0], [4.3, 4.0], [0.0, 4.0]], true);
        r.add_ring(vec![[4.3, 0.0], [10.0, 0.0], [10.0, 4.0], [4.3, 4.0]], true);
        // The same piece twice still counts once.
        r.add_ring(vec![[4.3, 0.0], [10.0, 0.0], [10.0, 4.0], [4.3, 4.0]], true);
        assert!(r.fill(4).iter().all(|&c| (c - 1.0).abs() < 1e-6));
    }

    #[test]
    fn partial_pixels_get_partial_coverage() {
        let mut r = Raster::new(4, 1);
        r.add_ring(vec![[0.0, 0.0], [1.25, 0.0], [1.25, 1.0], [0.0, 1.0]], true);
        let c = r.fill(4);
        assert!((c[0] - 1.0).abs() < 1e-6 && (c[1] - 0.25).abs() < 1e-6 && c[2] == 0.0);
    }

    #[test]
    fn projection_that_flips_winding_still_fills() {
        let mut r = Raster::new(10, 10);
        r.add_ring(vec![[1.0, 1.0], [1.0, 9.0], [9.0, 9.0], [9.0, 1.0]], true);
        assert!((sum(&r.fill(2)) - 64.0).abs() < 1e-3);
    }
}
