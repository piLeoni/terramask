//! What to read from the tiles: layers and classes of the OpenMapTiles
//! schema, by rule or by preset name.

use crate::Error;

/// Not a tile layer: each tile's square less its sea (`water`, class `ocean`).
pub const LAND: &str = "land";

/// Named selections, as rules: `layer:class,class` or `layer:*`.
pub const PRESETS: &[(&str, &str)] = &[
    ("water", "water:ocean,lake,river,dock waterway:river,canal,stream"),
    ("ocean", "water:ocean"),
    ("lakes", "water:lake"),
    ("rivers", "water:river waterway:river,stream"),
    ("land", LAND),
    ("forest", "landcover:wood"),
    ("glacier", "landcover:ice"),
    ("wetland", "landcover:wetland"),
    ("sand", "landcover:sand"),
    ("rock", "landcover:rock"),
    ("grass", "landcover:grass"),
    ("farmland", "landcover:farmland"),
    ("parks", "park:*"),
    ("urban", "landuse:residential,commercial,industrial,retail"),
];

/// Features of one layer whose class is listed; no classes keeps them all.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub layer: String,
    pub classes: Vec<String>,
}

impl Rule {
    pub fn new(layer: &str, classes: &[&str]) -> Self {
        Rule { layer: layer.into(), classes: classes.iter().map(|c| c.to_string()).collect() }
    }

    pub fn matches(&self, layer: &str, class: &str) -> bool {
        self.layer == layer && (self.classes.is_empty() || self.classes.iter().any(|c| c == class))
    }

    fn parse(s: &str) -> Result<Rule, Error> {
        if s == LAND {
            return Ok(Rule::new(LAND, &[]));
        }
        let (layer, classes) = s.split_once(':').ok_or_else(|| unknown(s))?;
        if layer.is_empty() {
            return Err(unknown(s));
        }
        if classes == "*" {
            return Ok(Rule::new(layer, &[]));
        }
        let classes: Vec<&str> = classes.split(',').filter(|c| !c.is_empty()).collect();
        if classes.is_empty() {
            return Err(Error::Input(format!("{s:?}: no classes; write {layer}:* for every feature of the layer")));
        }
        Ok(Rule::new(layer, &classes))
    }
}

fn unknown(s: &str) -> Error {
    let names: Vec<&str> = PRESETS.iter().map(|p| p.0).collect();
    Error::Input(format!("{s:?} is not a preset ({}) nor a layer:class rule", names.join(", ")))
}

/// Which features to keep. The default is water: the sea, lakes, rivers and
/// docks, and river, canal and stream centre lines.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub rules: Vec<Rule>,
    /// Keep water that is only there part of the year.
    pub intermittent: bool,
    /// Keep water that runs underground (culverts, covered channels).
    pub tunnels: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Filter::water(Filter::DEFAULT_AREAS, Filter::DEFAULT_LINES)
    }
}

impl Filter {
    /// Area classes of the default water (`water` layer).
    pub const DEFAULT_AREAS: &'static [&'static str] = &["ocean", "lake", "river", "dock"];
    /// Line classes of the default water (`waterway` layer).
    pub const DEFAULT_LINES: &'static [&'static str] = &["river", "canal", "stream"];

    /// Water of these area classes (`ocean`, `lake`, `river`, `dock`, `pond`,
    /// `swimming_pool`) and line classes (`river`, `canal`, `stream`,
    /// `ditch`, `drain`).
    pub fn water<A: AsRef<str>, L: AsRef<str>>(areas: &[A], lines: &[L]) -> Self {
        let rule = |layer: &str, c: Vec<String>| Rule { layer: layer.into(), classes: c };
        let mut rules = Vec::new();
        if !areas.is_empty() {
            rules.push(rule("water", areas.iter().map(|c| c.as_ref().to_string()).collect()));
        }
        if !lines.is_empty() {
            rules.push(rule("waterway", lines.iter().map(|c| c.as_ref().to_string()).collect()));
        }
        Filter { rules, intermittent: false, tunnels: false }
    }

    /// From preset names (see [`PRESETS`]) and rules: `layer:class,class`,
    /// or `layer:*` for every feature of a layer.
    ///
    /// ```
    /// let f = watermask::Filter::parse(&["forest", "parks", "landuse:cemetery"]).unwrap();
    /// assert!(f.matches("landcover", "wood") && f.matches("park", "nature_reserve"));
    /// ```
    pub fn parse<S: AsRef<str>>(items: &[S]) -> Result<Self, Error> {
        let mut rules: Vec<Rule> = Vec::new();
        for item in items {
            let item = item.as_ref().trim();
            let specs = PRESETS.iter().find(|p| p.0 == item).map_or(item, |p| p.1);
            for spec in specs.split_whitespace() {
                let r = Rule::parse(spec)?;
                match rules.iter_mut().find(|q| q.layer == r.layer) {
                    Some(q) if q.classes.is_empty() || r.classes.is_empty() => q.classes.clear(),
                    Some(q) => q.classes.extend(r.classes.into_iter().filter(|c| !q.classes.contains(c)).collect::<Vec<_>>()),
                    None => rules.push(r),
                }
            }
        }
        Ok(Filter { rules, intermittent: false, tunnels: false })
    }

    pub fn matches(&self, layer: &str, class: &str) -> bool {
        self.rules.iter().any(|r| r.matches(layer, class))
    }

    /// Whether land is asked for (see [`LAND`]).
    pub fn land(&self) -> bool {
        self.rules.iter().any(|r| r.layer == LAND)
    }

    /// The tile layers these rules read.
    pub fn layers(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in &self.rules {
            let l = if r.layer == LAND { "water" } else { r.layer.as_str() };
            if !out.iter().any(|o| o == l) {
                out.push(l.to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_rules() {
        let f = Filter::parse(&["water", "lakes", "water:pond", "parks", "land"]).unwrap();
        assert!(f.matches("water", "ocean") && f.matches("water", "pond") && f.matches("waterway", "canal"));
        assert!(f.matches("park", "anything") && !f.matches("landcover", "wood"));
        assert!(f.land());
        assert_eq!(f.layers(), ["water", "waterway", "park"]);
        assert_eq!(Filter::parse(&["water"]).unwrap(), Filter::default());
    }

    #[test]
    fn a_layer_with_every_class_absorbs_its_rules() {
        let f = Filter::parse(&["landcover:wood", "landcover:*", "landcover:ice"]).unwrap();
        assert_eq!(f.rules, vec![Rule::new("landcover", &[])]);
    }

    #[test]
    fn bad_names_say_what_is_allowed() {
        let e = Filter::parse(&["forrest"]).unwrap_err().to_string();
        assert!(e.contains("forest") && e.contains("layer:class"), "{e}");
        assert!(Filter::parse(&["landcover:"]).is_err());
        assert!(Filter::parse(&[":wood"]).is_err());
    }
}
