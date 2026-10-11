use terramask::{Mask, Ring};

fn square(x0: f64, y0: f64, s: f64, exterior: bool) -> Ring {
    Ring { exterior, points: vec![[x0, y0], [x0 + s, y0], [x0 + s, y0 + s], [x0, y0 + s]] }
}

#[test]
fn rings_from_anywhere_fill_and_measure() {
    let m = Mask::from_rings(20, 20, &[square(2.0, 2.0, 16.0, true), square(6.0, 6.0, 8.0, false)], 4);
    let covered: f64 = m.coverage.iter().map(|&c| c as f64).sum();
    assert!((covered - (256.0 - 64.0)).abs() < 1e-3, "the square less its hole");
    assert_eq!(m.coverage[3 * 20 + 3], 1.0);
    assert_eq!(m.coverage[10 * 20 + 10], 0.0, "the hole is a hole");

    let d = m.distance();
    assert!(d[3 * 20 + 3] > 0.0, "inside is positive");
    assert!(d[10 * 20 + 10] < 0.0, "the hole is outside");
    assert!(d[0] < 0.0, "outside is negative");
}

#[test]
fn either_winding_fills_the_same() {
    let cw = Ring { exterior: true, points: vec![[1.0, 1.0], [1.0, 9.0], [9.0, 9.0], [9.0, 1.0]] };
    let ccw = square(1.0, 1.0, 8.0, true);
    assert_eq!(Mask::from_rings(10, 10, &[cw], 2).coverage, Mask::from_rings(10, 10, &[ccw], 2).coverage);
}
