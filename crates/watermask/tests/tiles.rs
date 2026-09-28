//! Real OpenFreeMap tiles around Martha's Vineyard (zoom 12), read offline.

use std::collections::BTreeSet;
use std::io::Write;

use watermask::{merc_to_lonlat, Filter, GeoJsonOptions, Grid, MaskOptions, TileId, Water};

// Vineyard Sound: open water crossing the edge between two tiles.
const WEST: &[u8] = include_bytes!("fixtures/12-1243-1528.pbf");
const EAST: &[u8] = include_bytes!("fixtures/12-1244-1528.pbf");
// Inland Martha's Vineyard: ponds and a sliver of sea.
const ISLAND: &[u8] = include_bytes!("fixtures/12-1244-1529.pbf");
const OPEN_SEA: &[u8] = include_bytes!("fixtures/12-1244-1531.pbf");

fn water(tiles: &[(u32, u32, &[u8])]) -> Water {
    let mut w = Water::new();
    for &(x, y, b) in tiles {
        w.add_tile(TileId::new(12, x, y), b, &Filter::default()).unwrap();
    }
    w
}

/// A grid laid exactly over tiles x0..=x1 of row y, 256 px per tile.
fn grid_over(x0: u32, x1: u32, y: u32) -> Grid {
    let (a, b) = (TileId::new(12, x0, y).merc_bounds(), TileId::new(12, x1, y).merc_bounds());
    let [w, s] = merc_to_lonlat(a[0], a[1]);
    let [e, n] = merc_to_lonlat(b[2], b[3]);
    Grid::new([w, s, e, n], 256 * (x1 - x0 + 1) as usize, 256)
}

#[test]
fn open_sea_is_all_water() {
    let m = water(&[(1244, 1531, OPEN_SEA)]).mask(&grid_over(1244, 1244, 1531), &MaskOptions::default());
    assert!(m.water_fraction() > 0.999, "{}", m.water_fraction());
}

#[test]
fn reads_classes_from_a_coastal_tile() {
    let w = water(&[(1244, 1528, EAST)]);
    let classes: BTreeSet<&str> = w.areas.iter().map(|a| a.class.as_str()).collect();
    assert!(classes.contains("ocean") && classes.contains("lake"), "{classes:?}");
    assert!(!classes.contains("swimming_pool") && !classes.contains("pond"));
    let m = w.mask(&grid_over(1244, 1244, 1528), &MaskOptions::default());
    let f = m.water_fraction();
    assert!(f > 0.2 && f < 0.95, "coastal tile should be part water, got {f}");
}

#[test]
fn neighbouring_tiles_join_without_a_seam() {
    let m = water(&[(1243, 1528, WEST), (1244, 1528, EAST)]).mask(&grid_over(1243, 1244, 1528), &MaskOptions::default());
    let at = |x: usize, y: usize| m.coverage[y * m.width + x];
    // Wherever both sides of the tile edge are open water, so is the edge.
    let mut checked = 0;
    for y in 0..m.height {
        if at(254, y) == 1.0 && at(257, y) == 1.0 {
            checked += 1;
            assert_eq!((at(255, y), at(256, y)), (1.0, 1.0), "seam at row {y}");
        }
    }
    assert!(checked > 20, "only {checked} rows of open water across the edge");
    // And the shore does not break there: no outline runs along the edge.
    let along_edge = m.outlines().iter().flatten().filter(|p| (p[0] - 256.0).abs() < 0.01).count();
    assert!(along_edge < 10, "{along_edge} outline points on the tile edge");
}

#[test]
fn gzipped_tiles_read_the_same() {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(ISLAND).unwrap();
    let a = water(&[(1244, 1529, ISLAND)]);
    let b = water(&[(1244, 1529, &gz.finish().unwrap())]);
    assert_eq!(a.areas, b.areas);
    assert_eq!(a.lines, b.lines);
}

#[test]
fn filter_decides_what_is_water() {
    let g = grid_over(1244, 1244, 1529);
    let all = water(&[(1244, 1529, ISLAND)]).mask(&g, &MaskOptions::default()).water_fraction();
    let mut sea_only = Water::new();
    let f = Filter { areas: vec!["ocean".into()], ..Filter::default() };
    sea_only.add_tile(TileId::new(12, 1244, 1529), ISLAND, &f).unwrap();
    let sea = sea_only.mask(&g, &MaskOptions::default()).water_fraction();
    assert!(sea < all, "lakes add water: {sea} vs {all}");
}

#[test]
fn geojson_holds_every_feature() {
    let w = water(&[(1244, 1529, ISLAND)]);
    let pieces = w.to_geojson(&GeoJsonOptions { pieces: true, ..Default::default() });
    assert!(pieces.starts_with("{\"type\":\"FeatureCollection\""));
    assert_eq!(pieces.matches("\"type\":\"Feature\"").count(), w.areas.len() + w.lines.len());
    // Lon/lat around the Vineyard.
    assert!(pieces.contains("[-70.") && pieces.contains(",41."));
    let merged = w.to_geojson(&GeoJsonOptions::default());
    let classes: BTreeSet<&str> = w.areas.iter().map(|a| a.class.as_str()).collect();
    assert_eq!(merged.matches("\"type\":\"Feature\"").count(), classes.len() + w.lines.len());
}

fn diff(a: &watermask::Mask, b: &watermask::Mask) -> f32 {
    a.coverage.iter().zip(&b.coverage).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.coverage.len() as f32
}

#[test]
fn merged_areas_cover_the_same_water_in_one_piece_per_class() {
    let w = water(&[(1243, 1528, WEST), (1244, 1528, EAST)]);
    let m = w.merged(None);
    let classes: BTreeSet<&str> = w.areas.iter().map(|a| a.class.as_str()).collect();
    assert_eq!(m.areas.len(), classes.len());
    assert!(m.areas.iter().map(|a| a.rings.len()).sum::<usize>() < w.areas.iter().map(|a| a.rings.len()).sum::<usize>());
    let g = grid_over(1243, 1244, 1528);
    let d = diff(&w.mask(&g, &MaskOptions::default()), &m.mask(&g, &MaskOptions::default()));
    assert!(d < 1e-4, "coverage moved by {d}");
    // The sea crosses the tile edge, and no longer has a ring edge there.
    let edge = TileId::new(12, 1244, 1528).merc_bounds()[0];
    let ocean = m.areas.iter().find(|a| a.class == "ocean").unwrap();
    let on_edge = ocean.rings.iter().flat_map(|r| &r.points).filter(|p| (p[0] - edge).abs() < 0.01).count();
    assert!(on_edge < 10, "{on_edge} ocean points on the tile edge");
}

#[test]
fn bounds_cut_areas_and_lines() {
    let w = water(&[(1244, 1529, ISLAND)]);
    let g = grid_over(1244, 1244, 1529);
    let [west, south, east, north] = g.bounds;
    let half = [west, south, (west + east) / 2.0, north];
    for cut in [w.merged(Some(half)), w.clipped(half)] {
        let merc = Grid::new(half, 1, 1).merc;
        // The union snaps to a grid of about 2^30 steps across the area.
        let inside = |p: &&[f64; 2]| p[0] >= merc[0] - 1e-3 && p[0] <= merc[2] + 1e-3;
        assert!(cut.areas.iter().flat_map(|a| &a.rings).flat_map(|r| &r.points).all(|p| inside(&p)));
        assert!(cut.lines.iter().flat_map(|l| &l.points).all(|p| inside(&p)));
        // Same water on the kept half, none on the other.
        let (a, b) = (w.mask(&g, &MaskOptions::default()), cut.mask(&g, &MaskOptions::default()));
        let cols = |m: &watermask::Mask, xs: std::ops::Range<usize>| -> f32 {
            (0..m.height).map(|y| xs.clone().map(|x| m.coverage[y * m.width + x]).sum::<f32>()).sum()
        };
        assert!((cols(&a, 0..127) - cols(&b, 0..127)).abs() < 1.0);
        assert!(cols(&b, 129..256) < 1.0);
    }
}

#[test]
fn a_bad_tile_is_an_error_not_a_panic() {
    let mut w = Water::new();
    let e = w.add_tile(TileId::new(12, 1, 1), &[0x1a, 0xff, 0xff, 0xff], &Filter::default());
    assert!(matches!(e, Err(watermask::Error::Data(_))));
}
