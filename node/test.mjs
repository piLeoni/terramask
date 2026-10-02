// Offline tests on real OpenFreeMap tiles around Martha's Vineyard.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'

const wm = createRequire(import.meta.url)('./index.js')
const fixture = (z, x, y) => readFileSync(new URL(`../crates/watermask/tests/fixtures/${z}-${x}-${y}.pbf`, import.meta.url))

function tileBounds(z, x, y) {
  const n = 2 ** z
  const lat = (row) => (Math.atan(Math.sinh(Math.PI * (1 - (2 * row) / n))) * 180) / Math.PI
  return [(x / n) * 360 - 180, lat(y + 1), ((x + 1) / n) * 360 - 180, lat(y)]
}

test('open sea is water', () => {
  const w = new wm.Water()
  w.addTile(12, 1244, 1531, fixture(12, 1244, 1531))
  const m = w.mask(tileBounds(12, 1244, 1531), 256, 256)
  const c = m.coverage()
  assert.equal(c.length, 256 * 256)
  assert.ok(c.every((v) => v > 0.999))
})

test('shoreline, distance and lines on a coastal tile', () => {
  const w = new wm.Water()
  w.addTile(12, 1244, 1529, fixture(12, 1244, 1529))
  const b = tileBounds(12, 1244, 1529)
  const m = w.mask(b, 256, 256)
  const [pts, ends] = m.outlines()
  assert.ok(ends.length > 0 && pts.length === ends[ends.length - 1] * 2)
  const d = m.distance()
  const c = m.coverage()
  for (let i = 0; i < c.length; i++) assert.ok(c[i] >= 0.5 ? d[i] > 0 : d[i] < 0)
  assert.equal(w.lineClasses().length, w.lineCount)
  const g = JSON.parse(w.geojson({ pieces: true }))
  assert.equal(g.features.length, w.areaCount + w.lineCount)
})

test('geojson joins pieces and cuts to bounds', () => {
  const w = new wm.Water()
  w.addTile(12, 1243, 1528, fixture(12, 1243, 1528))
  w.addTile(12, 1244, 1528, fixture(12, 1244, 1528))
  const areas = JSON.parse(w.geojson()).features.filter((f) => f.geometry.type === 'MultiPolygon')
  const classes = areas.map((f) => f.properties.class)
  assert.equal(new Set(classes).size, classes.length)
  assert.ok(areas.length < w.areaCount)
  const b = tileBounds(12, 1244, 1528)
  const box = [b[0], b[1], (b[0] + b[2]) / 2, b[3]]
  const lons = JSON.parse(w.geojson({ bounds: box })).features.flatMap((f) => f.geometry.coordinates.flat(3).filter((_, i) => i % 2 === 0))
  assert.ok(lons.length > 0 && Math.min(...lons) >= box[0] - 1e-6 && Math.max(...lons) <= box[2] + 1e-6)
  assert.throws(() => w.geojson({ bounds: [0, 0, 1] }), /bounds/)
})

test('filter options', () => {
  const b = tileBounds(12, 1244, 1529)
  const frac = (w) => w.mask(b, 256, 256).coverage().reduce((a, v) => a + v, 0)
  const all = new wm.Water()
  all.addTile(12, 1244, 1529, fixture(12, 1244, 1529))
  const sea = new wm.Water()
  sea.addTile(12, 1244, 1529, fixture(12, 1244, 1529), { areas: ['ocean'] })
  assert.ok(frac(sea) < frac(all))
})

test('presets, land and subsets', () => {
  const b = tileBounds(12, 1244, 1529)
  const w = new wm.Water()
  w.addTile(12, 1244, 1529, fixture(12, 1244, 1529), { select: ['forest', 'land'] })
  const layers = new Set(w.areas().map((a) => a.layer))
  assert.deepEqual([...layers].sort(), ['land', 'landcover'])
  const woods = w.subset(['forest'])
  assert.ok(woods.areaCount > 0 && woods.areas().every((a) => a.class === 'wood'))
  const sea = new wm.Water()
  sea.addTile(12, 1244, 1529, fixture(12, 1244, 1529), { select: ['ocean'] })
  const s = sea.mask(b, 128, 128).coverage()
  const l = w.subset(['land']).mask(b, 128, 128).coverage()
  const mean = s.reduce((acc, v, i) => acc + v + l[i], 0) / s.length
  assert.ok(Math.abs(mean - 1) < 1e-3)
  assert.ok('glacier' in wm.presets())
  assert.throws(() => w.addTile(12, 1244, 1529, fixture(12, 1244, 1529), { select: ['forrest'] }), /forest/)
})

test('elevation and depth bands', () => {
  const b = tileBounds(12, 1244, 1531)
  const e = new wm.Elevation()
  e.addTile(12, 1244, 1531, readFileSync(new URL('../crates/watermask/tests/fixtures/dem-12-1244-1531.png', import.meta.url)))
  const h = e.grid(b, 64, 64)
  assert.equal(h.length, 64 * 64)
  assert.ok(h.every((v) => v < 5) && Math.min(...h) < -20)
  const sea = new wm.Water()
  sea.addTile(12, 1244, 1531, fixture(12, 1244, 1531), { select: ['ocean'] })
  const zones = sea.split(e, [-20, -10])
  const bands = zones.areas().map((a) => [a.low ?? null, a.high ?? null])
  assert.ok(bands.some(([lo, hi]) => lo === null && hi === -20), JSON.stringify(bands))
  const deep = zones.within(undefined, -20).mask(b, 64, 64).coverage()
  const both = zones.mask(b, 64, 64).coverage()
  const whole = sea.mask(b, 64, 64).coverage()
  assert.ok(both.every((v, i) => Math.abs(v - whole[i]) < 1e-3))
  assert.ok(deep.some((v) => v > 0.99))
  assert.throws(() => sea.split(new wm.Elevation(), [0]))
})

test('tiles and zoom', () => {
  assert.equal(wm.zoomFor([-70.85, 41.3, -70.45, 41.55], 1200), 13)
  assert.deepEqual(wm.tilesFor([4.9, 52.37, 4.9001, 52.3701], 12), [[12, 2103, 1346]])
})

test('bad input throws', () => {
  assert.throws(() => wm.fetch([0, 0, 1, 1]), /width/)
  assert.throws(() => new wm.Water().mask([0, 0, 1], 10), /bounds/)
  assert.throws(() => new wm.Water().addTile(12, 1, 1, Buffer.from([0x1a, 0xff, 0xff, 0xff])))
})
