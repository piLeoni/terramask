//! GeoJSON (RFC 7946) in lon/lat. Rings are written as given; the areas
//! passed in have been through `merge`, which winds exteriors
//! counter-clockwise and holes clockwise.

use std::fmt::Write;

use crate::{merc_to_lonlat, Water};

fn coords(out: &mut String, pts: &[[f64; 2]], close: bool) {
    out.push('[');
    let n = pts.len();
    let last = if close && n > 0 && pts[0] != pts[n - 1] { n + 1 } else { n };
    for i in 0..last {
        let p = pts[i % n];
        let [lon, lat] = merc_to_lonlat(p[0], p[1]);
        if i > 0 {
            out.push(',');
        }
        // 1e-7° is about a centimetre.
        let _ = write!(out, "[{},{}]", round7(lon), round7(lat));
    }
    out.push(']');
}

fn round7(v: f64) -> f64 {
    (v * 1e7).round() / 1e7
}

fn class(out: &mut String, c: &str) {
    let _ = write!(out, "\"properties\":{{\"class\":\"{}\"}}", c.replace('\\', "\\\\").replace('"', "\\\""));
}

pub fn write(w: &Water) -> String {
    let mut out = String::from("{\"type\":\"FeatureCollection\",\"features\":[");
    let mut first = true;
    let mut sep = |out: &mut String| {
        if !first {
            out.push(',');
        }
        first = false;
    };
    for a in &w.areas {
        // Each exterior with the holes that follow it.
        let mut polys: Vec<Vec<&[[f64; 2]]>> = Vec::new();
        for r in &a.rings {
            if r.exterior || polys.is_empty() {
                polys.push(vec![&r.points]);
            } else {
                polys.last_mut().unwrap().push(&r.points);
            }
        }
        sep(&mut out);
        out.push_str("{\"type\":\"Feature\",");
        class(&mut out, &a.class);
        out.push_str(",\"geometry\":{\"type\":\"MultiPolygon\",\"coordinates\":[");
        for (i, p) in polys.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('[');
            for (j, ring) in p.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                coords(&mut out, ring, true);
            }
            out.push(']');
        }
        out.push_str("]}}");
    }
    for l in &w.lines {
        sep(&mut out);
        out.push_str("{\"type\":\"Feature\",");
        class(&mut out, &l.class);
        out.push_str(",\"geometry\":{\"type\":\"LineString\",\"coordinates\":");
        coords(&mut out, &l.points, false);
        out.push_str("}}");
    }
    out.push_str("]}");
    out
}
