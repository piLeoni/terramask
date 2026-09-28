//! Marching squares over pixel centres, joined into polylines with water on
//! the left (x right, y down).

use std::collections::{HashMap, HashSet};

pub fn outlines(v: &[f32], w: usize, h: usize, level: f32) -> Vec<Vec<[f32; 2]>> {
    if w < 2 || h < 2 {
        return Vec::new();
    }
    let at = |x: usize, y: usize| v[y * w + x];
    let wet = |x: usize, y: usize| at(x, y) >= level;
    // Edge ids: horizontal edge from sample (x,y) to (x+1,y) is 2·(y·w+x),
    // vertical edge from (x,y) to (x,y+1) is 2·(y·w+x)+1.
    let point = |id: usize| -> [f32; 2] {
        let (i, vertical) = (id / 2, id % 2 == 1);
        let (x, y) = (i % w, i / w);
        let (x2, y2) = if vertical { (x, y + 1) } else { (x + 1, y) };
        let (a, b) = (at(x, y), at(x2, y2));
        let t = if a == b { 0.5 } else { ((level - a) / (b - a)).clamp(0.0, 1.0) };
        [x as f32 + 0.5 + t * (x2 as f32 - x as f32), y as f32 + 0.5 + t * (y2 as f32 - y as f32)]
    };
    // Segments by start edge; each edge starts at most one.
    let mut next: HashMap<usize, usize> = HashMap::new();
    let mut ends: HashSet<usize> = HashSet::new();
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            // Corners clockwise from top-left; edge k joins corner k and k+1.
            let c = [(x, y), (x + 1, y), (x + 1, y + 1), (x, y + 1)];
            let s = c.map(|(cx, cy)| wet(cx, cy));
            let edge = [2 * (y * w + x), 2 * (y * w + x + 1) + 1, 2 * ((y + 1) * w + x), 2 * (y * w + x) + 1];
            let crossed: Vec<usize> = (0..4).filter(|&k| s[k] != s[(k + 1) % 4]).collect();
            let mut add = |ka: usize, kb: usize, corner: usize| {
                // Orient so the corner's side (wet or dry) is where it belongs.
                let (pa, pb, pc) = (point(edge[ka]), point(edge[kb]), [c[corner].0 as f32 + 0.5, c[corner].1 as f32 + 0.5]);
                let cross = (pb[0] - pa[0]) * (pc[1] - pa[1]) - (pb[1] - pa[1]) * (pc[0] - pa[0]);
                let corner_left = cross < 0.0;
                let (a, b) = if corner_left == s[corner] { (edge[ka], edge[kb]) } else { (edge[kb], edge[ka]) };
                next.insert(a, b);
                ends.insert(b);
            };
            match crossed.len() {
                2 => {
                    // The corner between the two crossed edges is cut off.
                    let (k0, k1) = (crossed[0], crossed[1]);
                    let corner = if k1 == k0 + 1 { k1 } else { 0 };
                    add(k0, k1, corner);
                }
                4 => {
                    let centre = (at(x, y) + at(x + 1, y) + at(x + 1, y + 1) + at(x, y + 1)) / 4.0 >= level;
                    // Cut off the corners unlike the centre.
                    for corner in (0..4).filter(|&k| s[k] != centre) {
                        add((corner + 3) % 4, corner, corner);
                    }
                }
                _ => {}
            }
        }
    }
    let mut lines = Vec::new();
    let mut used: HashSet<usize> = HashSet::new();
    let walk = |start: usize, used: &mut HashSet<usize>| {
        let mut line = vec![point(start)];
        let mut e = start;
        while let Some(&n) = next.get(&e) {
            if !used.insert(e) {
                break;
            }
            line.push(point(n));
            e = n;
            if e == start {
                break;
            }
        }
        line
    };
    // Open lines start where nothing leads in; then the closed rings.
    let mut starts: Vec<usize> = next.keys().copied().filter(|k| !ends.contains(k)).collect();
    starts.sort_unstable();
    for s in starts {
        lines.push(walk(s, &mut used));
    }
    let mut rest: Vec<usize> = next.keys().copied().collect();
    rest.sort_unstable();
    for s in rest {
        if !used.contains(&s) {
            lines.push(walk(s, &mut used));
        }
    }
    lines.retain(|l| l.len() >= 2);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> Vec<f32> {
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
                (r - ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0)
            })
            .collect()
    }

    fn signed_area(l: &[[f32; 2]]) -> f32 {
        l.windows(2).map(|p| p[0][0] * p[1][1] - p[1][0] * p[0][1]).sum::<f32>() / 2.0
    }

    #[test]
    fn a_lake_is_one_closed_ring_with_water_on_the_left() {
        let v = disc(40, 40, 20.0, 20.0, 10.0);
        let o = outlines(&v, 40, 40, 0.5);
        assert_eq!(o.len(), 1);
        let ring = &o[0];
        assert_eq!(ring.first(), ring.last());
        // Water inside and on the left (y down) means counter-clockwise on
        // screen: negative shoelace area here.
        let a = signed_area(ring);
        assert!((a.abs() - std::f32::consts::PI * 100.0).abs() < 8.0, "area {a}");
        assert!(a < 0.0);
    }

    #[test]
    fn an_island_winds_the_other_way() {
        let v: Vec<f32> = disc(40, 40, 20.0, 20.0, 10.0).iter().map(|c| 1.0 - c).collect();
        let o = outlines(&v, 40, 40, 0.5);
        assert_eq!(o.len(), 1);
        assert!(signed_area(&o[0]) > 0.0);
    }

    #[test]
    fn a_coast_crossing_the_grid_is_open() {
        let v: Vec<f32> = (0..30 * 20).map(|i| if (i % 30) < 12 { 1.0 } else { 0.0 }).collect();
        let o = outlines(&v, 30, 20, 0.5);
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].len(), 20);
        // Water to the west, left of the line: it runs north (up the screen).
        assert!(o[0][0][1] > o[0][19][1]);
    }
}
