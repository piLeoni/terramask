//! What the tiles hold over a box: features per layer, class and subclass,
//! as tile pieces and once joined.
//!
//! ```sh
//! cargo run --example inventory --features fetch -- -122.266 37.871 -122.246 37.883 [zoom] [selector…]
//! ```

use std::collections::BTreeMap;

use terramask::{Features, Fetcher, Filter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        return Err("usage: inventory west south east north [zoom] [selector…]".into());
    }
    let n = |i: usize| args[i].parse::<f64>();
    let bounds = [n(0)?, n(1)?, n(2)?, n(3)?];
    let zoom: u8 = args.get(4).map(|z| z.parse()).transpose()?.unwrap_or(14);
    let select: Vec<&str> = args.iter().skip(5).map(String::as_str).collect();
    let all = ["building:*", "transportation:*", "landcover:*", "landuse:*", "park:*", "water:*", "waterway:*"];
    let filter = Filter::parse(if select.is_empty() { &all[..] } else { &select })?;
    let pieces = Fetcher::new().water(bounds, zoom, &filter, |_, _| {})?;
    let joined = pieces.joined(Some(bounds));
    let count = |f: &Features| {
        let mut m: BTreeMap<(String, String, String, &str), usize> = BTreeMap::new();
        for a in &f.areas {
            *m.entry((a.layer.clone(), a.class.clone(), a.subclass.clone(), "area")).or_default() += 1;
        }
        for l in &f.lines {
            *m.entry((l.layer.clone(), l.class.clone(), l.subclass.clone(), "line")).or_default() += 1;
        }
        m
    };
    let (before, after) = (count(&pieces), count(&joined));
    println!("{:<16} {:<14} {:<18} {:<5} {:>7} {:>7}", "layer", "class", "subclass", "", "pieces", "joined");
    for (k, v) in &before {
        println!("{:<16} {:<14} {:<18} {:<5} {:>7} {:>7}", k.0, k.1, k.2, k.3, v, after.get(k).copied().unwrap_or(0));
    }
    let tags: BTreeMap<&str, usize> = pieces.areas.iter().flat_map(|a| &a.tags).chain(pieces.lines.iter().flat_map(|l| &l.tags)).fold(BTreeMap::new(), |mut m, (k, _)| {
        *m.entry(k.as_str()).or_default() += 1;
        m
    });
    println!("\nattributes: {tags:?}");

    // Line ends left on a seam inside the fetched tiles: joins that failed.
    let ids = terramask::tiles_for(&bounds, zoom);
    let size = ids[0].merc_size();
    let (x0, y1) = (ids.iter().map(|t| t.merc_bounds()[0]).fold(f64::MAX, f64::min), ids.iter().map(|t| t.merc_bounds()[3]).fold(f64::MIN, f64::max));
    let (x1, y0) = (ids.iter().map(|t| t.merc_bounds()[2]).fold(f64::MIN, f64::max), ids.iter().map(|t| t.merc_bounds()[1]).fold(f64::MAX, f64::min));
    let seam = |v: f64, lo: f64, hi: f64| v > lo + 1.0 && v < hi - 1.0 && (((v - lo) / size).round() * size + lo - v).abs() < size * 1e-6;
    let open = |f: &Features| {
        f.lines
            .iter()
            .flat_map(|l| [l.points[0], l.points[l.points.len() - 1]])
            .filter(|p| seam(p[0], x0, x1) || seam(p[1], y0, y1))
            .count()
    };
    let whole = pieces.joined(None);
    println!("line ends on inner seams: {} as pieces, {} joined", open(&pieces), open(&whole));
    if std::env::var_os("SEAMS").is_some() {
        let ends: Vec<(&terramask::Line, [f64; 2])> = whole
            .lines
            .iter()
            .flat_map(|l| [(l, l.points[0]), (l, l.points[l.points.len() - 1])])
            .filter(|(_, p)| seam(p[0], x0, x1) || seam(p[1], y0, y1))
            .collect();
        for (l, p) in &ends {
            let near = ends
                .iter()
                .filter(|(m, q)| !std::ptr::eq(*m, *l) || q != p)
                .map(|(m, q)| ((q[0] - p[0]).hypot(q[1] - p[1]), m))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            match near {
                Some((d, m)) => println!("  {}/{}/{} {:?} — nearest {d:.2} m {}/{}/{} {:?}", l.layer, l.class, l.subclass, l.tags, m.class, m.subclass, m.layer, m.tags),
                None => println!("  {}/{}/{} alone", l.layer, l.class, l.subclass),
            }
        }
    }
    Ok(())
}
