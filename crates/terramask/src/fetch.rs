//! Download tiles over HTTP, with a disk cache.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::{tiles_for, Error, Features, Filter, Grid, TileId, ZoomLimits, MAX_ZOOM};
#[cfg(feature = "dem")]
use crate::{Elevation, TERRARIUM, TERRARIUM_ZOOM};

/// OpenFreeMap's planet tiles: OpenMapTiles schema, no key, weekly builds.
/// Attribution: "© OpenMapTiles © OpenStreetMap contributors".
pub const OPENFREEMAP: &str = "https://tiles.openfreemap.org/planet";

const AGENT: &str = concat!("terramask/", env!("CARGO_PKG_VERSION"));

/// How long a cached tile is used before it is downloaded again.
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);

pub struct Fetcher {
    /// A TileJSON URL, or a tile URL template with `{z}`, `{x}` and `{y}`.
    pub source: String,
    /// Terrain tiles (Terrarium PNG), a `{z}/{x}/{y}` template; only read by
    /// [`Fetcher::elevation`] and [`Fetcher::elevation_for`].
    #[cfg(feature = "dem")]
    pub elevation_source: String,
    /// Deepest terrain zoom read; [`TERRARIUM_ZOOM`] by default. Up to
    /// [`crate::TERRARIUM_MAX_ZOOM`] for detailed land heights: the sea
    /// floor flattened at those zooms is taken from zoom 10.
    #[cfg(feature = "dem")]
    pub elevation_max_zoom: u8,
    /// Where tiles are kept between runs; `None` downloads every time. Of
    /// vector tiles, only the layers asked for are stored, each on its own.
    pub cache: Option<PathBuf>,
    /// Cached tiles younger than this are used as they are, older ones
    /// downloaded again (or used anyway when offline), and deleted once a
    /// day when not asked for. Tiles are keyed by `source`, not by the build
    /// the TileJSON points to, so they outlive OpenFreeMap's weekly builds.
    pub max_age: Duration,
    /// Parallel downloads.
    pub threads: usize,
    /// One client for every request, so connections are reused rather than
    /// opened (TCP + TLS) per tile. Built on first use, sized to `threads`.
    agent: OnceLock<ureq::Agent>,
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new()
    }
}

/// A resolved tile source.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub template: String,
    pub max_zoom: u8,
}

impl Fetcher {
    /// OpenFreeMap, cached in [`default_cache`].
    pub fn new() -> Self {
        Fetcher {
            source: OPENFREEMAP.into(),
            #[cfg(feature = "dem")]
            elevation_source: TERRARIUM.into(),
            #[cfg(feature = "dem")]
            elevation_max_zoom: TERRARIUM_ZOOM,
            cache: Some(default_cache()),
            max_age: MAX_AGE,
            threads: 16,
            agent: OnceLock::new(),
        }
    }

    fn agent(&self) -> &ureq::Agent {
        self.agent.get_or_init(|| {
            ureq::AgentBuilder::new()
                .user_agent(AGENT)
                .max_idle_connections_per_host(self.threads.max(1))
                .timeout_connect(Duration::from_secs(15))
                .timeout_read(Duration::from_secs(60))
                .build()
        })
    }

    pub fn with_source(source: impl Into<String>) -> Self {
        Fetcher { source: source.into(), ..Self::new() }
    }

    /// The tile URL template and deepest zoom. A TileJSON is read fresh each
    /// time (its tile URLs change with every build) and the last good copy is
    /// used when offline.
    pub fn resolve(&self) -> Result<Source, Error> {
        if self.source.contains("{z}") {
            return Ok(Source { template: self.source.clone(), max_zoom: MAX_ZOOM });
        }
        let saved = self.cache.as_ref().map(|c| c.join("tilejson").join(format!("{}.json", slug(&self.source))));
        let text = match get(self.agent(), &self.source) {
            Ok(Some(body)) => {
                let text = String::from_utf8_lossy(&body).into_owned();
                if let Some(p) = &saved {
                    write_atomic(p, text.as_bytes());
                }
                text
            }
            Ok(None) => return Err(Error::Net(format!("{}: not found", self.source))),
            Err(e) => match saved.and_then(|p| std::fs::read_to_string(p).ok()) {
                Some(t) => t,
                None => return Err(e),
            },
        };
        parse_tilejson(&text).ok_or_else(|| Error::Data(format!("{}: no tile URL in the TileJSON", self.source)))
    }

    fn tile_dir(&self, source: &str, id: TileId) -> Option<PathBuf> {
        self.cache.as_ref().map(|c| c.join("tiles").join(slug(source)).join(id.z.to_string()).join(id.wrapped_x().to_string()))
    }

    /// One tile's `layers` (empty where the source has none of them) and
    /// whether they came from the cache. Each layer is cached in its own
    /// file, so a layer asked for later costs one download of the tile.
    pub fn tile(&self, source: &Source, id: TileId, layers: &[String]) -> Result<(Vec<u8>, bool), Error> {
        let files: Vec<PathBuf> = match self.tile_dir(&self.source, id) {
            Some(d) => layers.iter().map(|l| d.join(id.y.to_string()).join(format!("{}.pbf", slug(l)))).collect(),
            None => Vec::new(),
        };
        let cached = (!files.is_empty())
            .then(|| {
                let parts = files.iter().map(|p| std::fs::read(p).ok()).collect::<Option<Vec<_>>>()?;
                Some((parts.concat(), files.iter().all(|p| age(p) < self.max_age)))
            })
            .flatten();
        if let Some((bytes, true)) = cached {
            return Ok((bytes, true));
        }
        let url = tile_url(&source.template, id);
        let body = match get(self.agent(), &url) {
            Ok(body) => body.unwrap_or_default(),
            Err(e) => return cached.map(|(bytes, _)| (bytes, true)).ok_or(e),
        };
        let parts = crate::mvt::split_layers(&body, layers).map_err(|e| Error::Data(format!("{url}: {e}")))?;
        for (p, bytes) in files.iter().zip(&parts) {
            write_atomic(p, bytes);
        }
        Ok((parts.concat(), false))
    }

    /// Delete cached tiles older than `max_age`. Runs by itself at most once
    /// a day, after a download.
    pub fn prune(&self) {
        let Some(cache) = &self.cache else { return };
        let _ = std::fs::create_dir_all(cache);
        let _ = std::fs::write(cache.join(".pruned"), b"");
        prune_dir(&cache.join("tiles"), self.max_age);
    }

    fn prune_daily(&self) {
        let Some(cache) = &self.cache else { return };
        if age(&cache.join(".pruned")) > Duration::from_secs(24 * 3600) {
            self.prune();
        }
    }

    /// The features in the box that pass `filter`, from the tiles at zoom
    /// `z`. `progress(done, total)` is called as tiles arrive, from several
    /// threads.
    pub fn water(&self, bounds: [f64; 4], z: u8, filter: &Filter, progress: impl Fn(usize, usize) + Sync) -> Result<Features, Error> {
        let source = self.resolve()?;
        self.features_from(&source, &tiles_for(&bounds, z.min(source.max_zoom)), filter, progress)
    }

    /// The features for a grid, at the zoom that matches its resolution (see
    /// [`crate::zoom_for`]; the source's deepest zoom caps `limits`).
    pub fn features_for(
        &self,
        grid: &Grid,
        filter: &Filter,
        limits: &ZoomLimits,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<Features, Error> {
        let source = self.resolve()?;
        let z = grid.zoom(&ZoomLimits { max_zoom: limits.max_zoom.min(source.max_zoom), ..*limits });
        self.features_from(&source, &tiles_for(&grid.bounds, z), filter, progress)
    }

    fn features_from(
        &self,
        source: &Source,
        ids: &[TileId],
        filter: &Filter,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<Features, Error> {
        let layers = filter.layers();
        let tiles = self.fetch_all(ids, |id| self.tile(source, id, &layers).map(|(b, _)| b), progress)?;
        let mut out = Features::new();
        for (id, bytes) in ids.iter().zip(tiles) {
            out.add_tile(*id, &bytes, filter)?;
        }
        Ok(out)
    }

    /// One terrain tile's PNG (`None` where the source has none) and whether
    /// it came from the cache.
    #[cfg(feature = "dem")]
    pub fn elevation_tile(&self, id: TileId) -> Result<(Option<Vec<u8>>, bool), Error> {
        let path = self.tile_dir(&self.elevation_source, id).map(|d| d.join(format!("{}.png", id.y)));
        let cached = path.as_ref().and_then(|p| Some((std::fs::read(p).ok()?, age(p) < self.max_age)));
        let found = |b: Vec<u8>| (!b.is_empty()).then_some(b);
        if let Some((bytes, true)) = cached {
            return Ok((found(bytes), true));
        }
        let body = match get(self.agent(), &tile_url(&self.elevation_source, id)) {
            Ok(body) => body.unwrap_or_default(),
            Err(e) => return cached.map(|(bytes, _)| (found(bytes), true)).ok_or(e),
        };
        if let Some(p) = &path {
            write_atomic(p, &body);
        }
        Ok((found(body), false))
    }

    /// Terrain in the box from the tiles at zoom `z`, as given. Nothing is
    /// downloaded unless this is called.
    #[cfg(feature = "dem")]
    pub fn elevation(&self, bounds: [f64; 4], z: u8, progress: impl Fn(usize, usize) + Sync) -> Result<Elevation, Error> {
        self.elevation_from(&tiles_for(&bounds, z), progress)
    }

    /// Terrain for a grid, at the zoom that matches its resolution, capped
    /// at `elevation_max_zoom`.
    #[cfg(feature = "dem")]
    pub fn elevation_for(&self, grid: &Grid, limits: &ZoomLimits, progress: impl Fn(usize, usize) + Sync) -> Result<Elevation, Error> {
        let z = grid.zoom(&ZoomLimits { max_zoom: limits.max_zoom.min(self.elevation_max_zoom), ..*limits });
        self.elevation_from(&tiles_for(&grid.bounds, z), progress)
    }

    /// Deeper than [`TERRARIUM_ZOOM`], tiles with sea flattened to 0 m take
    /// the sea floor back from their zoom-10 ancestors (see
    /// [`Elevation::fill_sea`]); `progress` counts those tiles too.
    #[cfg(feature = "dem")]
    fn elevation_from(&self, ids: &[TileId], progress: impl Fn(usize, usize) + Sync) -> Result<Elevation, Error> {
        let mut out = self.elevation_tiles(ids, &progress)?;
        if out.zoom() > TERRARIUM_ZOOM {
            let mut up: Vec<TileId> = out.flat_sea_tiles().iter().map(|t| t.ancestor(TERRARIUM_ZOOM)).collect();
            up.sort();
            up.dedup();
            if !up.is_empty() {
                let n = ids.len();
                let coarser = self.elevation_tiles(&up, &|done, total| progress(n + done, n + total))?;
                out.fill_sea(&coarser);
            }
        }
        Ok(out)
    }

    #[cfg(feature = "dem")]
    fn elevation_tiles(&self, ids: &[TileId], progress: &(impl Fn(usize, usize) + Sync)) -> Result<Elevation, Error> {
        let tiles = self.fetch_all(ids, |id| self.elevation_tile(id).map(|(b, _)| b), progress)?;
        let mut out = Elevation::new();
        for (id, png) in ids.iter().zip(tiles) {
            if let Some(png) = png {
                out.add_tile(*id, &png)?;
            }
        }
        Ok(out)
    }

    /// `get` for every tile, on `threads` threads, results in order.
    fn fetch_all<T: Send>(
        &self,
        ids: &[TileId],
        get: impl Fn(TileId) -> Result<T, Error> + Sync,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<Vec<T>, Error> {
        let slots: Vec<Mutex<Option<Result<T, Error>>>> = ids.iter().map(|_| Mutex::new(None)).collect();
        let (next, done) = (AtomicUsize::new(0), AtomicUsize::new(0));
        std::thread::scope(|s| {
            for _ in 0..self.threads.clamp(1, ids.len().max(1)) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= ids.len() {
                        break;
                    }
                    let r = get(ids[i]);
                    *slots[i].lock().unwrap() = Some(r);
                    progress(done.fetch_add(1, Ordering::Relaxed) + 1, ids.len());
                });
            }
        });
        self.prune_daily();
        slots.into_iter().map(|s| s.into_inner().unwrap().expect("every tile was fetched")).collect()
    }
}

fn tile_url(template: &str, id: TileId) -> String {
    template.replace("{z}", &id.z.to_string()).replace("{x}", &id.wrapped_x().to_string()).replace("{y}", &id.y.to_string())
}

/// `$TERRAMASK_CACHE`, or the platform's cache folder: `~/Library/Caches/terramask`
/// on macOS, `%LOCALAPPDATA%\terramask` on Windows, `$XDG_CACHE_HOME/terramask`
/// or `~/.cache/terramask` elsewhere.
pub fn default_cache() -> PathBuf {
    let var = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(p) = var("TERRAMASK_CACHE") {
        return p;
    }
    let home = || var("HOME").or_else(|| var("USERPROFILE")).unwrap_or_default();
    let base = if cfg!(target_os = "macos") {
        home().join("Library").join("Caches")
    } else if cfg!(windows) {
        var("LOCALAPPDATA").unwrap_or_else(|| home().join("AppData").join("Local"))
    } else {
        var("XDG_CACHE_HOME").unwrap_or_else(|| home().join(".cache"))
    };
    base.join("terramask")
}

/// Time since the file was written; forever when it is missing.
fn age(path: &Path) -> Duration {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).unwrap_or(Duration::MAX)
}

fn prune_dir(dir: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => prune_dir(&p, max_age),
            Ok(_) if age(&p) > max_age => {
                let _ = std::fs::remove_file(&p);
            }
            _ => {}
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Body of a GET, `None` for 404/204.
fn get(agent: &ureq::Agent, url: &str) -> Result<Option<Vec<u8>>, Error> {
    match agent.get(url).call() {
        Ok(r) if r.status() == 204 => Ok(None),
        Ok(r) => {
            let mut body = Vec::new();
            r.into_reader().read_to_end(&mut body).map_err(|e| Error::Net(format!("{url}: {e}")))?;
            Ok(Some(body))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(Error::Net(format!("{url}: {e}"))),
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// A file name from a URL or template: scheme, query and everything from
/// `{z}` on dropped, the rest kept readable.
fn slug(url: &str) -> String {
    let s = url.split_once("://").map_or(url, |(_, r)| r);
    let s = s.split(['?', '#']).next().unwrap_or(s);
    let s = s.split("{z}").next().unwrap_or(s).replace(['{', '}'], "");
    s.trim_matches('/').chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

/// The first `tiles` URL and `maxzoom` of a TileJSON document.
fn parse_tilejson(text: &str) -> Option<Source> {
    let after = &text[text.find("\"tiles\"")? + 7..];
    let after = &after[after.find('[')? + 1..];
    let start = after.find('"')? + 1;
    let len = after[start..].find('"')?;
    let template = after[start..start + len].replace("\\/", "/");
    let max_zoom = text
        .find("\"maxzoom\"")
        .and_then(|i| {
            let rest = text[i + 9..].trim_start().strip_prefix(':')?.trim_start();
            let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            n.parse::<u8>().ok()
        })
        .unwrap_or(MAX_ZOOM);
    Some(Source { template, max_zoom })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tilejson() {
        let t = r#"{"tilejson":"3.0.0","tiles":["https:\/\/t.example\/p\/2026\/{z}\/{x}\/{y}.pbf"],"vector_layers":[{"id":"water","maxzoom":14}],"maxzoom":14,"minzoom":0}"#;
        let s = parse_tilejson(t).unwrap();
        assert_eq!(s.template, "https://t.example/p/2026/{z}/{x}/{y}.pbf");
        assert_eq!(s.max_zoom, 14);
    }

    #[test]
    fn cached_tiles_serve_offline_and_old_ones_are_pruned() {
        let dir = std::env::temp_dir().join(format!("terramask-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fetcher = Fetcher { cache: Some(dir.clone()), ..Fetcher::with_source("http://127.0.0.1:9/{z}/{x}/{y}.pbf") };
        let source = fetcher.resolve().unwrap();
        let (fresh, old, missing) = (TileId { z: 3, x: 1, y: 2 }, TileId { z: 3, x: 2, y: 2 }, TileId { z: 3, x: 3, y: 2 });
        let layers = ["water".to_string(), "waterway".to_string()];
        let file = |id: TileId, l: &str| dir.join("tiles").join("127.0.0.1_9").join(format!("{}/{}/{}/{l}.pbf", id.z, id.x, id.y));
        for (id, body) in [(fresh, "fresh"), (old, "stale")] {
            write_atomic(&file(id, "water"), body.as_bytes());
            write_atomic(&file(id, "waterway"), b"+lines");
        }
        let then = std::time::SystemTime::now() - MAX_AGE - Duration::from_secs(60);
        std::fs::File::options().write(true).open(file(old, "water")).unwrap().set_modified(then).unwrap();

        assert_eq!(fetcher.tile(&source, fresh, &layers).unwrap(), (b"fresh+lines".to_vec(), true));
        assert_eq!(fetcher.tile(&source, old, &layers).unwrap(), (b"stale+lines".to_vec(), true), "offline: the old copy");
        assert!(matches!(fetcher.tile(&source, missing, &layers), Err(Error::Net(_))));
        let more = ["water".to_string(), "park".to_string()];
        assert!(matches!(fetcher.tile(&source, fresh, &more), Err(Error::Net(_))), "a layer never stored");

        fetcher.prune();
        assert!(file(fresh, "water").exists());
        assert!(!file(old, "water").exists());
        assert!(age(&dir.join(".pruned")) < Duration::from_secs(60));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slugs_are_file_names() {
        assert_eq!(
            slug("https://tiles.openfreemap.org/planet/20260913_164504_pt/{z}/{x}/{y}.pbf"),
            "tiles.openfreemap.org_planet_20260913_164504_pt"
        );
        assert_eq!(slug("https://tiles.openfreemap.org/planet"), "tiles.openfreemap.org_planet");
    }
}
