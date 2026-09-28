//! Render the water of an area to a PNG: water grey, shoreline black,
//! waterway lines dark grey.
//!
//!     cargo run --release --example render -- -70.85,41.3,-70.45,41.55 1200 vineyard.png

use std::time::Instant;

use watermask::{Fetcher, Filter, Grid, MaskOptions, ZoomLimits};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: render WEST,SOUTH,EAST,NORTH WIDTH OUT.png");
        std::process::exit(2);
    }
    let b: Vec<f64> = args[1].split(',').map(|s| s.trim().parse()).collect::<Result<_, _>>()?;
    let bounds: [f64; 4] = b.try_into().map_err(|_| "bounds need 4 numbers")?;
    let width: usize = args[2].parse()?;
    let grid = Grid::with_width(bounds, width);

    let t = Instant::now();
    let fetcher = Fetcher::new();
    let limits = ZoomLimits::default();
    let source = fetcher.resolve()?;
    let z = grid.zoom(&ZoomLimits { max_zoom: limits.max_zoom.min(source.max_zoom), ..limits });
    let water = fetcher.water_for(&grid, &Filter::default(), &limits, |_, _| {})?;
    let fetched = t.elapsed();

    let t = Instant::now();
    let mask = water.mask(&grid, &MaskOptions::default());
    let outlines = mask.outlines();
    let lines = water.lines_on(&grid);
    let dist = mask.distance();
    let drawn = t.elapsed();

    let (w, h) = (grid.width, grid.height);
    let mut img: Vec<u8> = mask.coverage.iter().map(|&c| (255.0 - c * 60.0) as u8).collect();
    for l in &lines {
        stroke(&mut img, w, h, l, 110);
    }
    for l in &outlines {
        stroke(&mut img, w, h, l, 0);
    }
    let file = std::fs::File::create(&args[3])?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.write_header()?.write_image_data(&img)?;

    let far = dist.iter().filter(|d| d.is_finite()).fold(0f32, |m, &d| m.max(d));
    println!(
        "{w}×{h} px, zoom {z}, {} areas, {} lines · water {:.1}% · {} shore lines · farthest from shore {far:.0} px · fetch {:.2?}, mask+outlines+distance {:.2?}",
        water.areas.len(),
        water.lines.len(),
        mask.water_fraction() * 100.0,
        outlines.len(),
        fetched,
        drawn
    );
    Ok(())
}

/// Hairline polyline (DDA).
fn stroke(img: &mut [u8], w: usize, h: usize, pts: &[[f32; 2]], v: u8) {
    for s in pts.windows(2) {
        let (a, b) = (s[0], s[1]);
        let n = (b[0] - a[0]).abs().max((b[1] - a[1]).abs()).ceil().max(1.0) as usize;
        for i in 0..=n {
            let t = i as f32 / n as f32;
            let (x, y) = (a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]));
            if x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h {
                img[y as usize * w + x as usize] = v;
            }
        }
    }
}
