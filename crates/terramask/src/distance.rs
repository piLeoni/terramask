//! Exact Euclidean distance transform (Felzenszwalb & Huttenlocher), signed:
//! positive in water, negative on land, zero on the shore.

const FAR: f64 = 1e20;

pub fn signed(coverage: &[f32], w: usize, h: usize) -> Vec<f32> {
    let wet: Vec<bool> = coverage.iter().map(|&c| c >= 0.5).collect();
    let to_land = edt(&wet, w, h, false);
    let to_water = edt(&wet, w, h, true);
    (0..w * h)
        .map(|i| {
            // Pixel centres sit half a pixel from the shore between them.
            let (d, sign) = if wet[i] { (to_land[i], 1.0) } else { (to_water[i], -1.0) };
            if d >= FAR {
                sign * f32::INFINITY
            } else {
                sign * (d.sqrt() - 0.5) as f32
            }
        })
        .collect()
}

/// Squared distance from every pixel to the nearest pixel where wet == target.
fn edt(wet: &[bool], w: usize, h: usize, target: bool) -> Vec<f64> {
    let mut g: Vec<f64> = wet.iter().map(|&b| if b == target { 0.0 } else { FAR }).collect();
    let n = w.max(h);
    let (mut f, mut d, mut v, mut z) = (vec![0.0; n], vec![0.0; n], vec![0usize; n], vec![0.0; n + 1]);
    for x in 0..w {
        for y in 0..h {
            f[y] = g[y * w + x];
        }
        pass(&f[..h], &mut d[..h], &mut v, &mut z);
        for y in 0..h {
            g[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&g[y * w..(y + 1) * w]);
        pass(&f[..w], &mut d[..w], &mut v, &mut z);
        g[y * w..(y + 1) * w].copy_from_slice(&d[..w]);
    }
    g
}

/// 1-D squared distance transform of sampled function `f` into `d`.
fn pass(f: &[f64], d: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    let meet = |q: usize, p: usize| ((f[q] + (q * q) as f64) - (f[p] + (p * p) as f64)) / (2.0 * (q as f64 - p as f64));
    for q in 1..n {
        let mut s = meet(q, v[k]);
        // z[0] is -∞, so this stops at k = 0 at the latest.
        while s <= z[k] {
            k -= 1;
            s = meet(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, dq) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        *dq = (q as f64 - p as f64).powi(2) + f[p];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_across_a_straight_shore() {
        // Columns 0..5 land, 5..12 water.
        let (w, h) = (12, 3);
        let c: Vec<f32> = (0..w * h).map(|i| if i % w >= 5 { 1.0 } else { 0.0 }).collect();
        let d = signed(&c, w, h);
        let row: Vec<f32> = d[w..2 * w].to_vec();
        assert_eq!(row[5], 0.5);
        assert_eq!(row[11], 6.5);
        assert_eq!(row[4], -0.5);
        assert_eq!(row[0], -4.5);
    }

    #[test]
    fn diagonal_is_euclidean() {
        let (w, h) = (10, 10);
        let mut c = vec![1.0f32; w * h];
        c[0] = 0.0;
        let d = signed(&c, w, h);
        assert!((d[9 * w + 9] - ((81.0f32 * 2.0).sqrt() - 0.5)).abs() < 1e-4);
    }

    #[test]
    fn all_water_is_infinitely_far_from_shore() {
        assert!(signed(&[1.0; 4], 2, 2).iter().all(|d| d.is_infinite() && *d > 0.0));
    }
}
