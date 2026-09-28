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

use crate::mvt::clip_line;
use crate::{Area, Line, Ring};

fn signed_area(pts: &[[f64; 2]]) -> f64 {
    let n = pts.len();
    (0..n).map(|i| pts[i][0] * pts[(i + 1) % n][1] - pts[(i + 1) % n][0] * pts[i][1]).sum::<f64>() / 2.0
}

/// Exteriors counter-clockwise, holes clockwise (y up), without the closing point.
fn oriented(r: &Ring) -> Vec<[f64; 2]> {
    let mut pts = r.points.clone();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    if (signed_area(&pts) > 0.0) != r.exterior {
        pts.reverse();
    }
    pts
}

/// Areas of the same class as one area each when `merge`, else one per piece;
/// cut to `clip` (Mercator xmin, ymin, xmax, ymax) when given.
pub fn areas(areas: &[Area], merge: bool, clip: Option<[f64; 4]>) -> Vec<Area> {
    let mut groups: Vec<(&str, Vec<&Area>)> = Vec::new();
    for a in areas {
        match groups.iter_mut().find(|(c, _)| merge && *c == a.class) {
            Some((_, list)) => list.push(a),
            None => groups.push((&a.class, vec![a])),
        }
    }
    groups
        .into_iter()
        .filter_map(|(class, list)| {
            let subj: Vec<Vec<[f64; 2]>> = list.iter().flat_map(|a| &a.rings).map(oriented).filter(|r| r.len() >= 3).collect();
            let rings: Vec<Ring> = if !merge && clip.is_none() {
                list.iter().flat_map(|a| &a.rings).map(|r| Ring { exterior: r.exterior, points: oriented(r) }).collect()
            } else {
                // OGC-valid: no ring touches itself, so GEOS/shapely accept the output.
                let (ogc, solver) = (OverlayOptions::ogc(), Solver::default());
                let shapes = match clip {
                    Some([x0, y0, x1, y1]) => {
                        let rect = vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
                        FloatOverlay::with_subj_and_clip_custom(&subj, &rect, ogc, solver)
                            .overlay(OverlayRule::Intersect, FillRule::NonZero)
                    }
                    None => FloatOverlay::with_subj_custom(&subj, ogc, solver).overlay(OverlayRule::Subject, FillRule::NonZero),
                };
                shapes
                    .into_iter()
                    .flat_map(|shape| shape.into_iter().enumerate().map(|(i, points)| Ring { exterior: i == 0, points }))
                    .collect()
            };
            (!rings.is_empty()).then(|| Area { class: class.to_string(), rings })
        })
        .collect()
}

pub fn lines(lines: &[Line], clip: Option<[f64; 4]>) -> Vec<Line> {
    match clip {
        None => lines.to_vec(),
        Some(r) => {
            lines.iter().flat_map(|l| clip_line(&l.points, r).into_iter().map(|points| Line { class: l.class.clone(), points })).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64, exterior: bool) -> Ring {
        Ring { exterior, points: vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]] }
    }

    fn area(class: &str, rings: Vec<Ring>) -> Area {
        Area { class: class.into(), rings }
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
        let l = Line { class: "river".into(), points: vec![[0.0, 0.0], [10.0, 0.0]] };
        assert_eq!(lines(&[l], Some([5.0, -1.0, 20.0, 1.0]))[0].points, vec![[5.0, 0.0], [10.0, 0.0]]);
    }

    #[test]
    fn pieces_kept_apart_on_request() {
        let a = area("lake", vec![square(0.0, 0.0, 10.0, true)]);
        let b = area("lake", vec![square(10.0, 0.0, 10.0, true)]);
        assert_eq!(areas(&[a, b], false, None).len(), 2);
    }
}
