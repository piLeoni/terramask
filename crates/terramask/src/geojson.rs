//! GeoJSON (RFC 7946) in lon/lat. Rings are written as given; the areas
//! passed in have been through `merge`, which winds exteriors
//! counter-clockwise and holes clockwise.

use std::fmt::Write;

use crate::{merc_to_lonlat, Features};

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

fn quoted(s: &str) -> String {
    let mut q = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => q.push_str("\\\""),
            '\\' => q.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(q, "\\u{:04x}", c as u32);
            }
            c => q.push(c),
        }
    }
    q.push('"');
    q
}

fn properties(out: &mut String, layer: &str, class: &str, subclass: &str, tags: &[(String, String)], elevation: Option<[f64; 2]>) {
    let _ = write!(out, "\"properties\":{{\"layer\":{},\"class\":{}", quoted(layer), quoted(class));
    if !subclass.is_empty() {
        let _ = write!(out, ",\"subclass\":{}", quoted(subclass));
    }
    for (k, v) in tags.iter().filter(|(k, _)| !matches!(k.as_str(), "layer" | "class" | "subclass" | "min" | "max")) {
        let _ = write!(out, ",{}:{}", quoted(k), quoted(v));
    }
    if let Some(band) = elevation {
        for (k, v) in [("min", band[0]), ("max", band[1])] {
            if v.is_finite() {
                let _ = write!(out, ",\"{k}\":{v}");
            } else {
                let _ = write!(out, ",\"{k}\":null");
            }
        }
    }
    out.push('}');
}

pub fn write(w: &Features) -> String {
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
        properties(&mut out, &a.layer, &a.class, &a.subclass, &a.tags, a.elevation);
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
        properties(&mut out, &l.layer, &l.class, &l.subclass, &l.tags, None);
        out.push_str(",\"geometry\":{\"type\":\"LineString\",\"coordinates\":");
        coords(&mut out, &l.points, false);
        out.push_str("}}");
    }
    out.push_str("]}");
    out
}
