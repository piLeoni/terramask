//! Just enough of the Mapbox Vector Tile format (protobuf) to pull areas and
//! lines out of chosen layers. Geometry stays in tile units, cut to the tile
//! square so the buffer shared with neighbours is not counted twice.

use std::borrow::Cow;
use std::io::Read;

use crate::{Area, Filter, Line, Ring};

/// Geometry in tile units: 0..1 across, y down.
pub struct TileFeatures {
    pub areas: Vec<Area>,
    pub lines: Vec<Line>,
    /// The sea (`water`, class `ocean`), whatever the filter, when it asks for land.
    pub sea: Vec<Ring>,
}

fn gunzip(bytes: &[u8]) -> Result<Cow<'_, [u8]>, String> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(Cow::Borrowed(bytes));
    }
    let mut v = Vec::new();
    flate2::read::GzDecoder::new(bytes).read_to_end(&mut v).map_err(|e| format!("gzip: {e}"))?;
    Ok(Cow::Owned(v))
}

/// Each named layer of the tile on its own, uncompressed and framed as a
/// tile: empty where the tile lacks it. Tiles concatenate, so the parts can be
/// stored apart and joined again. Every feature is kept, so any [`Filter`]
/// on those layers still applies.
#[cfg(feature = "fetch")]
pub fn split_layers(bytes: &[u8], names: &[String]) -> Result<Vec<Vec<u8>>, String> {
    let bytes = gunzip(bytes)?;
    let mut out = vec![Vec::new(); names.len()];
    let mut r = Pbf::new(&bytes);
    while let Some((field, wire)) = r.key()? {
        if field == 3 && wire == 2 {
            let layer = r.bytes()?;
            let name = layer_name(layer)?;
            if let Some(i) = names.iter().position(|n| *n == name) {
                let o = &mut out[i];
                o.push(3 << 3 | 2);
                let mut n = layer.len() as u64;
                while n >= 0x80 {
                    o.push(n as u8 | 0x80);
                    n >>= 7;
                }
                o.push(n as u8);
                o.extend_from_slice(layer);
            }
        } else {
            r.skip(wire)?;
        }
    }
    Ok(out)
}

#[cfg(feature = "fetch")]
fn layer_name(buf: &[u8]) -> Result<String, String> {
    let mut r = Pbf::new(buf);
    while let Some((field, wire)) = r.key()? {
        if (field, wire) == (1, 2) {
            return r.string();
        }
        r.skip(wire)?;
    }
    Ok(String::new())
}

pub fn read(bytes: &[u8], filter: &Filter) -> Result<TileFeatures, String> {
    let bytes = gunzip(bytes)?;
    let mut out = TileFeatures { areas: Vec::new(), lines: Vec::new(), sea: Vec::new() };
    let mut r = Pbf::new(&bytes);
    while let Some((field, wire)) = r.key()? {
        if field == 3 && wire == 2 {
            read_layer(r.bytes()?, filter, &mut out)?;
        } else {
            r.skip(wire)?;
        }
    }
    Ok(out)
}

#[derive(Clone)]
enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
}

impl Value {
    fn truthy(&self) -> bool {
        match self {
            Value::Str(s) => !s.is_empty() && s != "0",
            Value::Num(n) => *n != 0.0,
            Value::Bool(b) => *b,
        }
    }
}

fn read_layer(buf: &[u8], filter: &Filter, out: &mut TileFeatures) -> Result<(), String> {
    // Fields can come in any order: collect, then interpret.
    let mut name = String::new();
    let mut features: Vec<&[u8]> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    let mut extent = 4096u64;
    let mut r = Pbf::new(buf);
    while let Some((field, wire)) = r.key()? {
        match (field, wire) {
            (1, 2) => name = r.string()?,
            (2, 2) => features.push(r.bytes()?),
            (3, 2) => keys.push(r.string()?),
            (4, 2) => values.push(read_value(r.bytes()?)?),
            (5, 0) => extent = r.varint()?,
            _ => r.skip(wire)?,
        }
    }
    let land = filter.land() && name == "water";
    if !land && !filter.rules.iter().any(|r| r.layer == name) {
        return Ok(());
    }
    let extent = extent.max(1) as f64;
    let unit = |pts: Vec<[f64; 2]>| pts.into_iter().map(|p| [p[0] / extent, p[1] / extent]).collect::<Vec<_>>();
    for f in features {
        let (tags, kind, geom) = read_feature(f)?;
        let mut class = String::new();
        let (mut intermittent, mut tunnel) = (false, false);
        for i in (1..tags.len()).step_by(2) {
            let (Some(k), Some(v)) = (keys.get(tags[i - 1] as usize), values.get(tags[i] as usize)) else { continue };
            match (k.as_str(), v) {
                ("class", Value::Str(s)) => class = s.clone(),
                ("intermittent", v) => intermittent = v.truthy(),
                ("brunnel", Value::Str(s)) => tunnel = s == "tunnel",
                _ => {}
            }
        }
        let sea = land && kind == 3 && class == "ocean";
        let keep = filter.matches(&name, &class) && !(intermittent && !filter.intermittent) && !(tunnel && !filter.tunnels);
        match kind {
            2 if keep => {
                for part in decode(&geom)? {
                    for piece in clip_line(&part, [0.0, 0.0, extent, extent]) {
                        out.lines.push(Line { layer: name.clone(), class: class.clone(), points: unit(piece) });
                    }
                }
            }
            3 if keep || sea => {
                let rings: Vec<Ring> = decode(&geom)?
                    .into_iter()
                    .filter_map(|ring| {
                        // The format marks exteriors by winding: positive area in
                        // tile coordinates (y down).
                        let exterior = area(&ring) > 0.0;
                        let points = clip_ring(&ring, extent);
                        (points.len() >= 3 && area(&points).abs() > 1e-9).then(|| Ring { exterior, points: unit(points) })
                    })
                    .collect();
                if sea {
                    out.sea.extend(rings.iter().cloned());
                }
                if keep && !rings.is_empty() {
                    out.areas.push(Area { layer: name.clone(), class, rings, elevation: None });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn read_value(buf: &[u8]) -> Result<Value, String> {
    let mut r = Pbf::new(buf);
    let mut v = Value::Num(0.0);
    while let Some((field, wire)) = r.key()? {
        v = match (field, wire) {
            (1, 2) => Value::Str(r.string()?),
            (2, 5) => Value::Num(f32::from_le_bytes(r.fixed::<4>()?) as f64),
            (3, 1) => Value::Num(f64::from_le_bytes(r.fixed::<8>()?)),
            (4, 0) => Value::Num(r.varint()? as i64 as f64),
            (5, 0) => Value::Num(r.varint()? as f64),
            (6, 0) => Value::Num(zigzag(r.varint()?) as f64),
            (7, 0) => Value::Bool(r.varint()? != 0),
            _ => {
                r.skip(wire)?;
                continue;
            }
        };
    }
    Ok(v)
}

/// (tags, geometry type, geometry commands).
fn read_feature(buf: &[u8]) -> Result<(Vec<u32>, u64, Vec<u32>), String> {
    let mut r = Pbf::new(buf);
    let (mut tags, mut kind, mut geom) = (Vec::new(), 0, Vec::new());
    while let Some((field, wire)) = r.key()? {
        match (field, wire) {
            (2, _) => r.packed_u32(wire, &mut tags)?,
            (3, 0) => kind = r.varint()?,
            (4, _) => r.packed_u32(wire, &mut geom)?,
            _ => r.skip(wire)?,
        }
    }
    Ok((tags, kind, geom))
}

fn zigzag(n: u64) -> i64 {
    ((n >> 1) as i64) ^ -((n & 1) as i64)
}

/// Geometry commands → parts (rings or lines), in tile units.
fn decode(cmds: &[u32]) -> Result<Vec<Vec<[f64; 2]>>, String> {
    let (mut x, mut y) = (0i64, 0i64);
    let mut parts: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut i = 0;
    while i < cmds.len() {
        let (id, count) = (cmds[i] & 7, (cmds[i] >> 3) as usize);
        i += 1;
        match id {
            1 | 2 => {
                if i + 2 * count > cmds.len() {
                    return Err("truncated geometry".into());
                }
                for _ in 0..count {
                    x += zigzag(cmds[i] as u64);
                    y += zigzag(cmds[i + 1] as u64);
                    i += 2;
                    if id == 1 {
                        parts.push(Vec::new());
                    }
                    parts.last_mut().ok_or("LineTo before MoveTo")?.push([x as f64, y as f64]);
                }
            }
            7 => {}
            _ => return Err(format!("unknown geometry command {id}")),
        }
    }
    Ok(parts)
}

/// Shoelace area, positive for rings the format calls exterior.
fn area(ring: &[[f64; 2]]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        / 2.0
}

/// Sutherland–Hodgman against the square [0, extent]². Keeps winding.
fn clip_ring(ring: &[[f64; 2]], extent: f64) -> Vec<[f64; 2]> {
    let mut pts = ring.to_vec();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    // (axis, keep >= bound?, bound)
    for (axis, keep_above, bound) in [(0, true, 0.0), (0, false, extent), (1, true, 0.0), (1, false, extent)] {
        if pts.is_empty() {
            break;
        }
        let inside = |p: &[f64; 2]| if keep_above { p[axis] >= bound } else { p[axis] <= bound };
        let mut out = Vec::with_capacity(pts.len() + 4);
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            let (ia, ib) = (inside(&a), inside(&b));
            if ia {
                out.push(a);
            }
            if ia != ib {
                let t = (bound - a[axis]) / (b[axis] - a[axis]);
                let mut p = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])];
                p[axis] = bound;
                out.push(p);
            }
        }
        pts = out;
    }
    pts
}

/// Cut a polyline to the box [xmin, ymin, xmax, ymax], splitting it where it leaves.
pub(crate) fn clip_line(line: &[[f64; 2]], r: [f64; 4]) -> Vec<Vec<[f64; 2]>> {
    let mut pieces = Vec::new();
    let mut cur: Vec<[f64; 2]> = Vec::new();
    for w in line.windows(2) {
        match clip_segment(w[0], w[1], r) {
            Some((a, b, cut_end)) => {
                if cur.last() != Some(&a) {
                    if cur.len() >= 2 {
                        pieces.push(std::mem::take(&mut cur));
                    }
                    cur = vec![a];
                }
                cur.push(b);
                if cut_end {
                    pieces.push(std::mem::take(&mut cur));
                }
            }
            None => {
                if cur.len() >= 2 {
                    pieces.push(std::mem::take(&mut cur));
                }
                cur.clear();
            }
        }
    }
    if cur.len() >= 2 {
        pieces.push(cur);
    }
    pieces
}

/// Liang–Barsky. Returns the visible part and whether its end was cut.
fn clip_segment(a: [f64; 2], b: [f64; 2], r: [f64; 4]) -> Option<([f64; 2], [f64; 2], bool)> {
    let d = [b[0] - a[0], b[1] - a[1]];
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-d[0], a[0] - r[0]), (d[0], r[2] - a[0]), (-d[1], a[1] - r[1]), (d[1], r[3] - a[1])] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    let at = |t: f64| {
        if t == 0.0 {
            a
        } else if t == 1.0 {
            b
        } else {
            [a[0] + t * d[0], a[1] + t * d[1]]
        }
    };
    Some((at(t0), at(t1), t1 < 1.0))
}

/// Protobuf wire-format reader.
struct Pbf<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Pbf<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Pbf { buf, pos: 0 }
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.pos).ok_or("truncated varint")?;
            self.pos += 1;
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err("varint too long".into())
    }

    fn key(&mut self) -> Result<Option<(u64, u8)>, String> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let k = self.varint()?;
        Ok(Some((k >> 3, (k & 7) as u8)))
    }

    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.varint()? as usize;
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len()).ok_or("truncated field")?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn string(&mut self) -> Result<String, String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let s = self.buf.get(self.pos..self.pos + N).ok_or("truncated field")?;
        self.pos += N;
        Ok(s.try_into().unwrap())
    }

    fn packed_u32(&mut self, wire: u8, out: &mut Vec<u32>) -> Result<(), String> {
        if wire == 2 {
            let mut inner = Pbf::new(self.bytes()?);
            while inner.pos < inner.buf.len() {
                out.push(inner.varint()? as u32);
            }
        } else {
            out.push(self.varint()? as u32);
        }
        Ok(())
    }

    fn skip(&mut self, wire: u8) -> Result<(), String> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => self.pos += 8,
            2 => {
                self.bytes()?;
            }
            5 => self.pos += 4,
            _ => return Err(format!("unsupported wire type {wire}")),
        }
        if self.pos > self.buf.len() {
            return Err("truncated field".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_clipped_to_the_tile_keeps_its_winding() {
        let big = [[-100.0, -100.0], [5000.0, -100.0], [5000.0, 5000.0], [-100.0, 5000.0]];
        let c = clip_ring(&big, 4096.0);
        assert_eq!(c.len(), 4);
        assert!((area(&c) - 4096.0 * 4096.0).abs() < 1e-6);
        assert_eq!(area(&big) > 0.0, area(&c) > 0.0);
    }

    #[test]
    fn line_leaving_and_reentering_splits() {
        let l = [[10.0, 10.0], [5000.0, 10.0], [5000.0, 20.0], [10.0, 20.0]];
        let p = clip_line(&l, [0.0, 0.0, 4096.0, 4096.0]);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0], vec![[10.0, 10.0], [4096.0, 10.0]]);
        assert_eq!(p[1], vec![[4096.0, 20.0], [10.0, 20.0]]);
    }

    #[test]
    #[cfg(feature = "fetch")]
    fn split_layers_decode_like_the_whole_tile() {
        let full = include_bytes!("../tests/fixtures/12-1244-1528.pbf");
        let all = Filter::parse(&["water:ocean,lake,pond", "waterway:stream", "landcover:*", "land"]).unwrap();
        let names = all.layers();
        assert_eq!(names, ["water", "waterway", "landcover"]);
        let parts = split_layers(full, &names).unwrap();
        let slim = parts.concat();
        assert!(parts[..2].concat().len() < full.len() / 2, "{} of {}", slim.len(), full.len());
        let (a, b) = (read(full, &all).unwrap(), read(&slim, &all).unwrap());
        let pts = |w: &TileFeatures| w.areas.iter().flat_map(|a| &a.rings).map(|r| r.points.len()).sum::<usize>();
        assert_eq!((a.areas.len(), a.lines.len(), a.sea.len(), pts(&a)), (b.areas.len(), b.lines.len(), b.sea.len(), pts(&b)));
        assert!(pts(&a) > 0 && !a.sea.is_empty());
        assert!(split_layers(&[], &names).unwrap().iter().all(|p| p.is_empty()));
    }

    #[test]
    fn geometry_commands() {
        // MoveTo(2,2) LineTo(+2,0)(0,+2) ClosePath — from the spec's examples.
        let cmds = [9, 4, 4, 18, 4, 0, 0, 4, 15];
        assert_eq!(decode(&cmds).unwrap(), vec![vec![[2.0, 2.0], [4.0, 2.0], [4.0, 4.0]]]);
    }
}
