//! Tile pieces joined into whole polygons, and cut to a box.
//!
//! Neighbouring tiles describe a shared edge with independently rounded
//! points, so the pieces are joined with a real polygon union rather than by
//! matching edges. The fill rule is nonzero, as for the mask, so the polygons
//! cover exactly what the mask covers.

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::core::solver::Solver;
use i_overlay::float::overlay::{FloatOverlay, OverlayOptions};

use std::collections::HashMap;

use crate::mvt::clip_line;
use crate::{Area, Line, Pin, Ring, TileId};

pub(crate) type Path = Vec<[f64; 2]>;

pub(crate) fn signed_area(pts: &[[f64; 2]]) -> f64 {
    let n = pts.len();
    (0..n).map(|i| pts[i][0] * pts[(i + 1) % n][1] - pts[(i + 1) % n][0] * pts[i][1]).sum::<f64>() / 2.0
}

/// Exteriors counter-clockwise, holes clockwise (y up), without the closing point.
fn oriented(r: &Ring) -> Path {
    let mut pts = r.points.clone();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    if (signed_area(&pts) > 0.0) != r.exterior {
        pts.reverse();
    }
    pts
}

/// Rings as paths for [`overlay`], wound so the nonzero rule fills the area.
pub(crate) fn paths<'a>(rings: impl IntoIterator<Item = &'a Ring>) -> Vec<Path> {
    rings.into_iter().map(oriented).filter(|r| r.len() >= 3).collect()
}

pub(crate) fn rect([x0, y0, x1, y1]: [f64; 4]) -> Vec<Path> {
    vec![vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]]
}

/// A boolean operation under the nonzero rule. The output is OGC-valid (no
/// ring touches itself, so GEOS/shapely accept it), each exterior followed
/// by its holes.
pub(crate) fn overlay(subj: &[Path], clip: &[Path], rule: OverlayRule) -> Vec<Ring> {
    if subj.is_empty() || (clip.is_empty() && rule == OverlayRule::Intersect) {
        return Vec::new();
    }
    let (ogc, solver) = (OverlayOptions::ogc(), Solver::default());
    let shapes = if clip.is_empty() {
        FloatOverlay::with_subj_custom(subj, ogc, solver).overlay(OverlayRule::Subject, FillRule::NonZero)
    } else {
        FloatOverlay::with_subj_and_clip_custom(subj, clip, ogc, solver).overlay(rule, FillRule::NonZero)
    };
    shapes.into_iter().flat_map(|shape| shape.into_iter().enumerate().map(|(i, points)| Ring { exterior: i == 0, points })).collect()
}

/// Areas of the same layer, class and elevation band as one area each when
/// `merge`, else one per piece; cut to `clip` (Mercator xmin, ymin, xmax,
/// ymax) when given. A merged area keeps the subclass and tags its pieces
/// share.
pub fn areas(areas: &[Area], merge: bool, clip: Option<[f64; 4]>) -> Vec<Area> {
    let mut groups: Vec<(&Area, Vec<&Area>)> = Vec::new();
    for a in areas {
        match groups.iter_mut().find(|(g, _)| merge && g.layer == a.layer && g.class == a.class && g.elevation == a.elevation) {
            Some((_, list)) => list.push(a),
            None => groups.push((a, vec![a])),
        }
    }
    groups
        .into_iter()
        .filter_map(|(first, list)| {
            let rings = solid(list.iter().flat_map(|a| &a.rings), merge, clip);
            if rings.is_empty() {
                return None;
            }
            let mut out = Area { rings, ..first.without_rings() };
            if merge {
                if list.iter().any(|a| a.subclass != first.subclass) {
                    out.subclass.clear();
                }
                out.tags.retain(|t| list.iter().all(|a| a.tags.contains(t)));
            } else {
                out.tile = first.tile;
            }
            Some(out)
        })
        .collect()
}

/// Rings oriented for output; through the overlay when they need joining
/// (`union`) or cutting (`clip`).
fn solid<'a>(rings: impl Iterator<Item = &'a Ring>, union: bool, clip: Option<[f64; 4]>) -> Vec<Ring> {
    if !union && clip.is_none() {
        return rings.map(|r| Ring { exterior: r.exterior, points: oriented(r) }).filter(|r| r.points.len() >= 3).collect();
    }
    let subj = paths(rings);
    match clip {
        Some(c) => overlay(&subj, &rect(c), OverlayRule::Intersect),
        None => overlay(&subj, &[], OverlayRule::Subject),
    }
}

/// Each exterior ring with the holes that follow it.
fn polygons(rings: Vec<Ring>) -> Vec<Vec<Ring>> {
    let mut out: Vec<Vec<Ring>> = Vec::new();
    for r in rings {
        match out.last_mut() {
            Some(p) if !r.exterior => p.push(r),
            _ => out.push(vec![r]),
        }
    }
    out
}

/// Whether `p` lies on an edge of the tile that cut its feature: clipping
/// puts the cut points exactly there.
fn on_tile_edge(tile: Option<TileId>, p: [f64; 2]) -> bool {
    let Some(t) = tile else { return false };
    let [x0, y0, x1, y1] = t.merc_bounds();
    let eps = t.merc_size() * 1e-7;
    (p[0] - x0).abs() < eps || (p[0] - x1).abs() < eps || (p[1] - y0).abs() < eps || (p[1] - y1).abs() < eps
}

/// What a feature is, apart from where: pieces that agree on it can be one
/// feature.
type Kind<'a> = (&'a str, &'a str, &'a str, &'a [(String, String)]);

/// See [`crate::Features::joined`]. Pieces the tile cut (a ring on its tile's
/// edge) are joined with the cut pieces of the same kind they touch; whole
/// pieces pass through.
pub fn join_areas(areas: &[Area], clip: Option<[f64; 4]>) -> Vec<Area> {
    let mut out = Vec::new();
    let mut emit = |head: &Area, rings: Vec<Ring>| {
        for p in polygons(rings) {
            out.push(Area { rings: p, ..head.without_rings() });
        }
    };
    let mut cut: HashMap<(Kind, Option<[u64; 2]>), Vec<&Area>> = HashMap::new();
    let mut order = Vec::new();
    for a in areas {
        if a.rings.iter().flat_map(|r| &r.points).any(|&p| on_tile_edge(a.tile, p)) {
            let key = ((a.layer.as_str(), a.class.as_str(), a.subclass.as_str(), a.tags.as_slice()), a.elevation.map(|e| e.map(f64::to_bits)));
            let list = cut.entry(key).or_default();
            if list.is_empty() {
                order.push(key);
            }
            list.push(a);
        } else {
            emit(a, solid(a.rings.iter(), false, clip));
        }
    }
    for key in order {
        let list = &cut[&key];
        emit(list[0], solid(list.iter().flat_map(|a| &a.rings), true, clip));
    }
    out
}

/// See [`crate::Features::joined`].
pub fn join_lines(lines: &[Line], clip: Option<[f64; 4]>) -> Vec<Line> {
    let mut groups: HashMap<Kind, Vec<&Line>> = HashMap::new();
    let mut order = Vec::new();
    for l in lines.iter().filter(|l| l.points.len() >= 2) {
        let key = (l.layer.as_str(), l.class.as_str(), l.subclass.as_str(), l.tags.as_slice());
        let list = groups.entry(key).or_default();
        if list.is_empty() {
            order.push(key);
        }
        list.push(l);
    }
    let mut out = Vec::new();
    for key in order {
        let list = &groups[&key];
        for points in chain(list) {
            let head = Line { tile: None, ..list[0].without_points() };
            match clip {
                None => out.push(Line { points, ..head }),
                Some(r) => out.extend(clip_line(&points, r).into_iter().map(|points| Line { points, ..head.clone() })),
            }
        }
    }
    out
}

/// The pieces of one kind of line strung together where cut ends from
/// different tiles meet (within about four units of a 4096 tile: each tile
/// rounds and simplifies its side of the cut on its own).
fn chain(pieces: &[&Line]) -> Vec<Vec<[f64; 2]>> {
    let end = |k: usize| {
        let p = &pieces[k / 2].points;
        if k % 2 == 0 {
            p[0]
        } else {
            p[p.len() - 1]
        }
    };
    let tol = pieces.iter().filter_map(|l| l.tile).map(|t| t.merc_size() * 1e-3).fold(0.0, f64::max);
    let mut partner: Vec<Option<usize>> = vec![None; pieces.len() * 2];
    if tol > 0.0 {
        let cell = |p: [f64; 2]| ((p[0] / tol).floor() as i64, (p[1] / tol).floor() as i64);
        let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        let cut: Vec<usize> = (0..pieces.len() * 2).filter(|&k| on_tile_edge(pieces[k / 2].tile, end(k))).collect();
        for &k in &cut {
            grid.entry(cell(end(k))).or_default().push(k);
        }
        let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
        for &a in &cut {
            let (cx, cy) = cell(end(a));
            for (dx, dy) in [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 0), (0, 1), (1, -1), (1, 0), (1, 1)] {
                for &b in grid.get(&(cx + dx, cy + dy)).into_iter().flatten() {
                    if b > a && pieces[a / 2].tile != pieces[b / 2].tile {
                        let (p, q) = (end(a), end(b));
                        let d = (p[0] - q[0]).hypot(p[1] - q[1]);
                        if d <= tol {
                            pairs.push((d, a, b));
                        }
                    }
                }
            }
        }
        pairs.sort_by(|x, y| x.0.total_cmp(&y.0));
        for (_, a, b) in pairs {
            if partner[a].is_none() && partner[b].is_none() {
                partner[a] = Some(b);
                partner[b] = Some(a);
            }
        }
    }

    let mut seen = vec![false; pieces.len()];
    let mut out = Vec::new();
    // Open chains from their free ends first, then whatever closes on itself.
    let starts = (0..pieces.len() * 2).filter(|&k| partner[k].is_none()).chain(0..pieces.len() * 2);
    for start in starts {
        if seen[start / 2] {
            continue;
        }
        let mut pts: Vec<[f64; 2]> = Vec::new();
        let mut k = start;
        loop {
            seen[k / 2] = true;
            let p = &pieces[k / 2].points;
            let skip = usize::from(!pts.is_empty());
            if k % 2 == 0 {
                pts.extend(p.iter().skip(skip));
            } else {
                pts.extend(p.iter().rev().skip(skip));
            }
            match partner[k ^ 1] {
                Some(next) if !seen[next / 2] => k = next,
                Some(_) => {
                    pts.push(pts[0]);
                    break;
                }
                None => break,
            }
        }
        out.push(pts);
    }
    out
}

pub fn lines(lines: &[Line], clip: Option<[f64; 4]>) -> Vec<Line> {
    match clip {
        None => lines.to_vec(),
        Some(r) => lines
            .iter()
            .flat_map(|l| {
                let head = l.without_points();
                clip_line(&l.points, r).into_iter().map(move |points| Line { points, ..head.clone() })
            })
            .collect(),
    }
}

/// Points inside the clip rectangle (inclusive), Mercator metres.
pub(crate) fn clip_pins(pins: &[Pin], clip: Option<[f64; 4]>) -> Vec<Pin> {
    let Some([x0, y0, x1, y1]) = clip else {
        return pins.to_vec();
    };
    pins.iter()
        .filter(|p| {
            let [x, y] = p.position;
            x >= x0 && x <= x1 && y >= y0 && y <= y1
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64, exterior: bool) -> Ring {
        Ring { exterior, points: vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]] }
    }

    fn area(class: &str, rings: Vec<Ring>) -> Area {
        Area::new("water", class, rings)
    }

    /// A square of `tile` units inside tile (16, 100+tx, 200), in its own
    /// unit grid (0..1 across, y up from the tile's south edge).
    fn in_tile(tx: u32, x: f64, y: f64, s: f64) -> Area {
        let t = TileId::new(16, 100 + tx, 200);
        let [x0, y0, ..] = t.merc_bounds();
        let m = t.merc_size();
        let ring = square(x0 + x * m, y0 + y * m, s * m, true);
        Area { tile: Some(t), ..Area::new("building", "", vec![ring]) }
    }

    #[test]
    fn joined_mends_cut_pieces_and_keeps_neighbours_apart() {
        // A building cut by the seam between two tiles, and two whole ones
        // sharing a wall inside the left tile.
        let left = in_tile(0, 0.9, 0.4, 0.1);
        let right = in_tile(1, 0.0, 0.4, 0.05);
        let a = in_tile(0, 0.2, 0.2, 0.1);
        let b = in_tile(0, 0.3, 0.2, 0.1);
        let out = join_areas(&[left.clone(), a, b, right], None);
        assert_eq!(out.len(), 3, "the cut building is one, the neighbours two");
        let m = left.tile.unwrap().merc_size();
        let joined = out.iter().find(|x| x.rings.len() == 1 && (signed_area(&x.rings[0].points) - 0.0125 * m * m).abs() < 1e-6 * m * m);
        assert!(joined.is_some(), "a 0.1 square and a 0.05 one, as one ring");
        assert!(out.iter().all(|x| x.tile.is_none() && x.layer == "building"));
    }

    #[test]
    fn joined_strings_lines_across_the_seam() {
        let (t0, t1) = (TileId::new(16, 100, 200), TileId::new(16, 101, 200));
        let x = t0.merc_bounds()[2];
        let y = t0.merc_bounds()[1] + 10.0;
        let road = |t: TileId, pts: Vec<[f64; 2]>| Line { tile: Some(t), subclass: "residential".into(), ..Line::new("transportation", "minor", pts) };
        // The right piece runs backwards, and its cut end is rounded a bit off.
        let pieces = [road(t0, vec![[x - 50.0, y], [x, y]]), road(t1, vec![[x + 50.0, y + 5.0], [x, y + 0.01]]), road(t1, vec![[x, y + 30.0], [x + 9.0, y + 30.0]])];
        let out = join_lines(&pieces, None);
        assert_eq!(out.len(), 2, "two roads: one mended, one alone (its cut end has no partner)");
        let long = out.iter().find(|l| l.points.len() == 3).expect("mended road");
        assert_eq!(long.points[0], [x - 50.0, y]);
        assert_eq!(long.points[2], [x + 50.0, y + 5.0]);
        assert_eq!(long.subclass, "residential");
    }

    #[test]
    fn pieces_of_a_class_join_across_the_tile_edge() {
        // Two tiles' halves of one lake, the shared edge rounded differently.
        let left = area("lake", vec![Ring { exterior: true, points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]] }]);
        let right = area("lake", vec![Ring { exterior: true, points: vec![[10.0 - 1e-9, 0.0], [20.0, 0.0], [20.0, 10.0], [10.0, 10.0]] }]);
        let sea = area("ocean", vec![square(50.0, 50.0, 5.0, true)]);
        let out = areas(&[left, sea, right], true, None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].class, "lake");
        assert_eq!(out[0].rings.len(), 1, "one ring: no seam left");
        assert!((signed_area(&out[0].rings[0].points) - 200.0).abs() < 1e-6, "counter-clockwise, 20×10");
    }

    #[test]
    fn islands_stay_holes_and_wind_clockwise() {
        let lake = area("lake", vec![square(0.0, 0.0, 10.0, true), square(4.0, 4.0, 2.0, false)]);
        let out = areas(&[lake], true, None);
        let r = &out[0].rings;
        assert_eq!(r.len(), 2);
        assert!(r[0].exterior && !r[1].exterior);
        assert!(signed_area(&r[0].points) > 0.0 && signed_area(&r[1].points) < 0.0);
    }

    #[test]
    fn cut_to_a_box() {
        let lake = area("lake", vec![square(0.0, 0.0, 10.0, true)]);
        let out = areas(std::slice::from_ref(&lake), true, Some([5.0, -5.0, 15.0, 5.0]));
        assert!((signed_area(&out[0].rings[0].points) - 25.0).abs() < 1e-6);
        assert!(areas(&[lake], true, Some([20.0, 20.0, 30.0, 30.0])).is_empty());
        let l = Line::new("waterway", "river", vec![[0.0, 0.0], [10.0, 0.0]]);
        assert_eq!(lines(&[l], Some([5.0, -1.0, 20.0, 1.0]))[0].points, vec![[5.0, 0.0], [10.0, 0.0]]);
    }

    #[test]
    fn pieces_kept_apart_on_request() {
        let a = area("lake", vec![square(0.0, 0.0, 10.0, true)]);
        let b = area("lake", vec![square(10.0, 0.0, 10.0, true)]);
        assert_eq!(areas(&[a, b], false, None).len(), 2);
    }
}
