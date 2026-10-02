//! The images in the README, from live OpenFreeMap tiles.
//!
//!     cargo run --release --example readme              # writes docs/*.png
//!     cargo run --release --example readme -- venice    # only docs/venice.png

use watermask::{Fetcher, Filter, Grid, Mask, MaskOptions, Water, ZoomLimits};

use tiny_skia::{Color, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

type Lines = Vec<Vec<[f32; 2]>>;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    std::fs::create_dir_all(&out)?;
    let only: Vec<String> = std::env::args().skip(1).collect();
    let want = |name: &str| only.is_empty() || only.iter().any(|o| o == name);

    // Cape Cod, Martha's Vineyard and Nantucket, engraved: lines that follow
    // the shore, further apart out at sea, from Mask::distance.
    if want("cape-cod") {
        waterlines(&out.join("cape-cod.png"), [-71.05, 41.22, -69.9, 41.86], 1600)?;
    }

    // Water areas grey, shoreline black, waterway centre lines dark grey. All
    // the same size, to sit side by side.
    let (w, h) = (800, 800);
    for (name, lon, lat, span) in [("amsterdam", 4.898, 52.372, 0.085), ("venice", 12.34, 45.43, 0.16), ("stockholm", 18.06, 59.325, 0.2)] {
        if want(name) {
            plain(&out.join(format!("{name}.png")), Grid::new(around(lon, lat, span, w, h), w, h))?;
        }
    }

    // Martha's Vineyard in layers: the sea cut into depth zones and the land
    // into heights by terrain tiles, the woods from the landcover layer.
    if want("vineyard-zones") {
        zones(&out.join("vineyard-zones.png"), Grid::with_width([-70.85, 41.3, -70.45, 41.55], 1200))?;
    }
    Ok(())
}

fn zones(path: &std::path::Path, grid: Grid) -> Result<(), Box<dyn std::error::Error>> {
    let fetcher = Fetcher::new();
    let limits = ZoomLimits::default();
    let f = Filter::parse(&["ocean", "land", "forest"])?;
    let all = fetcher.water_for(&grid, &f, &limits, |_, _| {})?;
    // The sea floor from the default terrain zoom, the hills from deeper.
    let sea_floor = fetcher.elevation_for(&grid, &limits, |_, _| {})?;
    let mut detailed = Fetcher::new();
    detailed.elevation_max_zoom = watermask::TERRARIUM_MAX_ZOOM;
    let hills = detailed.elevation_for(&grid, &limits, |_, _| {})?;
    let sea = all.subset(&Filter::parse(&["ocean"])?).split(&sea_floor, &[-30.0, -20.0, -10.0, -5.0])?;
    let land = all.subset(&Filter::parse(&["land"])?).split(&hills, &[15.0, 30.0, 50.0])?;
    let woods = all.subset(&Filter::parse(&["forest"])?);

    let opts = MaskOptions::default();
    let mut pm = Pixmap::new(grid.width as u32, grid.height as u32).unwrap();
    pm.fill(Color::WHITE);
    let fill = |pm: &mut Pixmap, part: &Water, rgb: [u8; 3], alpha: f32| {
        let m = part.mask(&grid, &opts);
        for (px, &c) in pm.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(&m.coverage) {
            let a = c * alpha;
            for k in 0..3 {
                px[k] = (px[k] as f32 * (1.0 - a) + rgb[k] as f32 * a) as u8;
            }
        }
    };
    let deep = [(f64::NEG_INFINITY, -30.0), (-30.0, -20.0), (-20.0, -10.0), (-10.0, -5.0), (-5.0, f64::INFINITY)];
    for (i, &(lo, hi)) in deep.iter().enumerate() {
        let t = i as f32 / 4.0;
        let mix = |a: f32, b: f32| (a + (b - a) * t) as u8;
        fill(&mut pm, &sea.within(lo, hi), [mix(96.0, 222.0), mix(138.0, 235.0), mix(168.0, 242.0)], 1.0);
    }
    for (i, &(lo, hi)) in [(f64::NEG_INFINITY, 15.0), (15.0, 30.0), (30.0, 50.0), (50.0, f64::INFINITY)].iter().enumerate() {
        let v = 246.0 - 16.0 * i as f32;
        fill(&mut pm, &land.within(lo, hi), [v as u8, (v - 4.0) as u8, (v - 14.0) as u8], 1.0);
    }
    fill(&mut pm, &woods, [74, 120, 64], 0.45);
    let edges = |part: &Water| part.mask(&grid, &opts).outlines();
    for level in [-30.0, -20.0, -10.0, -5.0] {
        stroke(&mut pm, &edges(&sea.within(f64::NEG_INFINITY, level)), 0.5, 255);
    }
    stroke(&mut pm, &edges(&all.subset(&Filter::parse(&["ocean"])?)), 1.3, 0);
    frame(&mut pm);
    pm.save_png(path)?;
    println!("{}: {}×{}", path.display(), grid.width, grid.height);
    Ok(())
}

/// Bounds centred on lon, lat, `span` degrees wide, with the aspect of w×h.
fn around(lon: f64, lat: f64, span: f64, w: usize, h: usize) -> [f64; 4] {
    let [x, y] = watermask::lonlat_to_merc(lon, lat);
    let half_w = watermask::lonlat_to_merc(span / 2.0, 0.0)[0];
    let half_h = half_w * h as f64 / w as f64;
    let [west, south] = watermask::merc_to_lonlat(x - half_w, y - half_h);
    let [east, north] = watermask::merc_to_lonlat(x + half_w, y + half_h);
    [west, south, east, north]
}

fn fetch(grid: &Grid) -> Result<Water, watermask::Error> {
    Fetcher::new().water_for(grid, &Filter::default(), &ZoomLimits::default(), |_, _| {})
}

fn waterlines(path: &std::path::Path, bounds: [f64; 4], width: usize) -> Result<(), Box<dyn std::error::Error>> {
    // Work at twice the size so the distance field, which is measured
    // between pixel centres, gives smooth lines once scaled down.
    const K: f32 = 2.0;
    let grid = Grid::with_width(bounds, width * K as usize);
    let water = fetch(&grid)?;
    let sea = Water { areas: water.areas.iter().filter(|a| a.class == "ocean").cloned().collect(), lines: Vec::new() };
    let opts = MaskOptions::default();
    let dist = sea.mask(&grid, &opts).distance();

    let (w, h) = (width as u32, (grid.height as f32 / K).round() as u32);
    let mut pm = Pixmap::new(w, h).unwrap();
    pm.fill(Color::WHITE);

    const FAR: f32 = 150.0;
    for k in 1.. {
        let d = 2.4 * (k as f32).powf(1.32);
        if d > FAR {
            break;
        }
        let band =
            Mask { width: grid.width, height: grid.height, coverage: dist.iter().map(|&v| (v - d * K + 0.5).clamp(0.0, 1.0)).collect() };
        let grey = ((d / FAR).powf(0.7) * 200.0) as u8;
        stroke(&mut pm, &scale(band.outlines(), 1.0 / K), 0.55, grey);
    }
    let inland = Water { areas: water.areas.iter().filter(|a| a.class != "ocean").cloned().collect(), lines: Vec::new() };
    stroke(&mut pm, &scale(inland.mask(&grid, &opts).outlines(), 1.0 / K), 0.6, 90);
    stroke(&mut pm, &scale(sea.mask(&grid, &opts).outlines(), 1.0 / K), 1.3, 0);
    frame(&mut pm);
    pm.save_png(path)?;
    println!("{}: {w}×{h}", path.display());
    Ok(())
}

fn plain(path: &std::path::Path, grid: Grid) -> Result<(), Box<dyn std::error::Error>> {
    let water = fetch(&grid)?;
    let mask = water.mask(&grid, &MaskOptions::default());
    let mut pm = Pixmap::new(grid.width as u32, grid.height as u32).unwrap();
    let data = pm.data_mut();
    for (i, &c) in mask.coverage.iter().enumerate() {
        let v = (255.0 - c * 38.0) as u8;
        data[4 * i..4 * i + 4].copy_from_slice(&[v, v, v, 255]);
    }
    stroke(&mut pm, &water.lines_on(&grid), 0.7, 120);
    stroke(&mut pm, &mask.outlines(), 1.3, 0);
    frame(&mut pm);
    pm.save_png(path)?;
    println!("{}: {}×{}", path.display(), grid.width, grid.height);
    Ok(())
}

fn scale(lines: Lines, k: f32) -> Lines {
    lines.into_iter().map(|l| l.into_iter().map(|[x, y]| [x * k, y * k]).collect()).collect()
}

fn stroke(pm: &mut Pixmap, lines: &Lines, width: f32, grey: u8) {
    let mut pb = PathBuilder::new();
    for l in lines.iter().filter(|l| l.len() >= 2) {
        pb.move_to(l[0][0], l[0][1]);
        for p in &l[1..] {
            pb.line_to(p[0], p[1]);
        }
    }
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(grey, grey, grey, 255);
    paint.anti_alias = true;
    let st = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
    pm.stroke_path(&path, &paint, &st, Transform::identity(), None);
}

fn frame(pm: &mut Pixmap) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let edge = vec![vec![[0.5, 0.5], [w - 0.5, 0.5], [w - 0.5, h - 0.5], [0.5, h - 0.5], [0.5, 0.5]]];
    stroke(pm, &edge, 1.0, 0);
}
