//! The shared house file, `.lthouse` (screen 23 §12.1): a design and a drawing of its plan, as JSON.
//!
//! It holds no disc data. The textures are named by the design's style and colour keys and built
//! again from the importer's own disc, and the library's thumbnail is rendered on import, so files
//! can be passed round freely. A file the importer's Living Tool can't build (an unknown style, a
//! prop or a piece of furniture it doesn't stock, a newer format) is refused with the reason.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::design::{self, HouseDesign, Side, WallKind};
use super::{inside, style, EditError};

/// The format this Living Tool writes and reads.
pub const FORMAT: u32 = 1;

/// The file name extension.
pub const EXTENSION: &str = "lthouse";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HouseFile {
    /// The format version ([`FORMAT`]).
    pub lthouse: u32,
    /// What wrote it, e.g. "Living Tool 0.1.22".
    #[serde(default)]
    pub app: String,
    pub design: HouseDesign,
    /// The ground floor's plan, drawn as SVG ([`plan_svg`]): for a file browser or a web page.
    #[serde(default)]
    pub plan: String,
}

/// The file for `d`, written by `app`.
pub fn write(d: &HouseDesign, app: &str) -> Result<Vec<u8>, EditError> {
    let f = HouseFile { lthouse: FORMAT, app: app.to_string(), design: d.clone(), plan: plan_svg(d) };
    serde_json::to_vec_pretty(&f).map_err(|e| EditError::new("LTHOUSE_WRITE", e.to_string()))
}

/// Reads a file and checks this Living Tool can build its design ([`check`]).
pub fn read(bytes: &[u8]) -> Result<HouseFile, EditError> {
    #[derive(Deserialize)]
    struct Head {
        lthouse: Option<u32>,
        #[serde(default)]
        app: String,
    }
    let head: Head = serde_json::from_slice(bytes).map_err(|_| EditError::new("LTHOUSE_BAD", "This isn't a house design file (.lthouse)."))?;
    match head.lthouse {
        None => return Err(EditError::new("LTHOUSE_BAD", "This isn't a house design file (.lthouse).")),
        Some(v) if v > FORMAT => {
            let by = if head.app.is_empty() { String::new() } else { format!(" by {}", head.app) };
            return Err(EditError::new("LTHOUSE_NEWER", format!("This design was saved{by} in a newer format ({v}). Update Living Tool to open it.")));
        }
        Some(_) => {}
    }
    let f: HouseFile = serde_json::from_slice(bytes).map_err(|e| EditError::new("LTHOUSE_BAD", format!("The design in this file doesn't read: {e}")))?;
    check(&f.design)?;
    Ok(f)
}

/// Whether this Living Tool can build `d`: its style, its yard's props and its furniture are ones
/// it has, and the plan has no errors.
pub fn check(d: &HouseDesign) -> Result<(), EditError> {
    if style::style(&d.style).is_none() {
        let known: Vec<&str> = style::ALL.iter().map(|s| s.name).collect();
        return Err(EditError::new("LTHOUSE_STYLE", format!("It's built in the style “{}”, which this Living Tool doesn't have ({}).", d.style, known.join(", "))));
    }
    for p in d.yard.iter().flat_map(|y| &y.props) {
        if !design::YARD_PROPS.contains(&p.item.as_str()) {
            return Err(EditError::new("LTHOUSE_ITEM", format!("Its yard has a “{}”, which isn't a prop this Living Tool stocks.", p.item)));
        }
    }
    for f in d.inside.iter().flat_map(|i| &i.furniture) {
        if inside::item_size(&f.item).is_none() {
            return Err(EditError::new("LTHOUSE_ITEM", format!("Its inside has a “{}”, which isn't furniture this Living Tool stocks.", f.item)));
        }
    }
    if let Some(p) = design::validate(d).into_iter().find(|p| p.error) {
        return Err(EditError::new("LTHOUSE_PLAN", format!("Its plan has an error: {}", p.message)));
    }
    Ok(())
}

/// Pixels a module.
const M: i32 = 24;
/// Pixels round the drawing.
const PAD: i32 = 12;

/// The ground floor's plan as SVG, north up: the plot, the wings' modules and their outline, the
/// doors and windows, the porches and the chimneys.
pub fn plan_svg(d: &HouseDesign) -> String {
    let plan = design::footprint(d);
    let (mut lo, mut hi) = design::house_bounds(d);
    if let Some(y) = &d.yard {
        lo = [lo[0].min(y.at[0] as i16), lo[1].min(y.at[1] as i16)];
        hi = [hi[0].max((y.at[0] + y.size[0]) as i16), hi[1].max((y.at[1] + y.size[1]) as i16)];
    }
    if plan.is_empty() {
        (lo, hi) = ([0, 0], [1, 1]);
    }
    let x = |m: i16| (m - lo[0]) as i32 * M + PAD;
    let y = |m: i16| (hi[1] - m) as i32 * M + PAD;
    let (w, h) = ((hi[0] - lo[0]) as i32 * M + 2 * PAD, (hi[1] - lo[1]) as i32 * M + 2 * PAD);
    let mut s = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">"#);
    if let Some(yd) = &d.yard {
        let (ax, az) = (yd.at[0] as i16, yd.at[1] as i16);
        let dash = if yd.fence { "" } else { r#" stroke-dasharray="4 3""# };
        s += &format!(
            r##"<rect x="{}" y="{}" width="{}" height="{}" fill="#e3e8d6" stroke="#8c9a74" stroke-width="1.5"{dash}/>"##,
            x(ax),
            y(az + yd.size[1] as i16),
            yd.size[0] as i32 * M,
            yd.size[1] as i32 * M
        );
    }
    for m in plan.keys() {
        s += &format!(r##"<rect x="{}" y="{}" width="{M}" height="{M}" fill="#d8c9ab"/>"##, x(m[0]), y(m[1] + 1));
    }
    // The outline: each module side with no module across it.
    let ends = |m: [i16; 2], side: Side| -> [(i32, i32); 2] {
        match side {
            Side::North => [(x(m[0]), y(m[1] + 1)), (x(m[0] + 1), y(m[1] + 1))],
            Side::South => [(x(m[0]), y(m[1])), (x(m[0] + 1), y(m[1]))],
            Side::East => [(x(m[0] + 1), y(m[1] + 1)), (x(m[0] + 1), y(m[1]))],
            Side::West => [(x(m[0]), y(m[1] + 1)), (x(m[0]), y(m[1]))],
        }
    };
    let across = |m: [i16; 2], side: Side| match side {
        Side::North => [m[0], m[1] + 1],
        Side::East => [m[0] + 1, m[1]],
        Side::South => [m[0], m[1] - 1],
        Side::West => [m[0] - 1, m[1]],
    };
    for &m in plan.keys() {
        for side in Side::ALL {
            if !plan.contains_key(&across(m, side)) {
                let [(x0, y0), (x1, y1)] = ends(m, side);
                s += &format!(r##"<line x1="{x0}" y1="{y0}" x2="{x1}" y2="{y1}" stroke="#4a3d2c" stroke-width="2.5" stroke-linecap="square"/>"##);
            }
        }
    }
    // Doors and windows on the ground floor, a mark on their wall's middle half.
    let kinds: BTreeMap<([i16; 2], Side), WallKind> = d.walls.iter().filter(|w| w.storey == 0).map(|w| (([w.at[0] as i16, w.at[1] as i16], w.side), w.kind)).collect();
    for ((m, side), kind) in kinds {
        if !plan.contains_key(&m) || plan.contains_key(&across(m, side)) {
            continue;
        }
        let [(x0, y0), (x1, y1)] = ends(m, side);
        let (qx0, qy0, qx1, qy1) = (x0 + (x1 - x0) / 4, y0 + (y1 - y0) / 4, x1 - (x1 - x0) / 4, y1 - (y1 - y0) / 4);
        let (colour, width) = match kind {
            WallKind::Door => ("#a8452c", 5),
            WallKind::Window => ("#6f9cc4", 4),
            WallKind::Logs => continue,
        };
        s += &format!(r#"<line x1="{qx0}" y1="{qy0}" x2="{qx1}" y2="{qy1}" stroke="{colour}" stroke-width="{width}"/>"#);
    }
    for e in &d.extras {
        let f = e.front();
        let dash = if e.storey == 0 { "" } else { r#" stroke-dasharray="3 2""# };
        s += &format!(r##"<rect x="{}" y="{}" width="{}" height="{}" fill="none" stroke="#4a3d2c" stroke-width="1"{dash}/>"##, x(f[0]) + 3, y(f[1] + 1) + 3, M - 6, M - 6);
    }
    for c in &d.chimneys {
        s += &format!(r##"<rect x="{}" y="{}" width="8" height="8" fill="#4a3d2c"/>"##, x(c[0] as i16) + M / 2 - 4, y(c[1] as i16 + 1) + M / 2 - 4);
    }
    s += "</svg>";
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_goes_out_and_comes_back() {
        for (id, d) in design::presets() {
            let bytes = write(&d, "Living Tool test").unwrap();
            let back = read(&bytes).unwrap_or_else(|e| panic!("{id}: {}", e.message));
            assert_eq!(serde_json::to_string(&back.design).unwrap(), serde_json::to_string(&d).unwrap(), "{id}");
            assert!(back.plan.starts_with("<svg") && back.plan.contains("<line"), "{id}");
        }
    }

    #[test]
    fn what_this_living_tool_cant_build_is_refused_with_the_reason() {
        let (_, d) = design::presets().into_iter().next().unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&write(&d, "x").unwrap()).unwrap();
        v["design"]["style"] = "adobe-pueblo".into();
        let e = read(&serde_json::to_vec(&v).unwrap()).unwrap_err();
        assert_eq!(e.code, "LTHOUSE_STYLE");
        assert!(e.message.contains("adobe-pueblo"), "{}", e.message);

        v["lthouse"] = 2.into();
        assert_eq!(read(&serde_json::to_vec(&v).unwrap()).unwrap_err().code, "LTHOUSE_NEWER");
        assert_eq!(read(b"{\"name\": \"a house\"}").unwrap_err().code, "LTHOUSE_BAD");
        assert_eq!(read(b"not json").unwrap_err().code, "LTHOUSE_BAD");
    }

    #[test]
    fn an_unstocked_prop_is_refused() {
        let d = design::presets().into_iter().map(|(_, d)| d).find(|d| d.yard.as_ref().is_some_and(|y| !y.props.is_empty())).expect("a preset with props");
        let mut v: serde_json::Value = serde_json::from_slice(&write(&d, "x").unwrap()).unwrap();
        v["design"]["yard"]["props"][0]["item"] = "jukebox".into();
        let e = read(&serde_json::to_vec(&v).unwrap()).unwrap_err();
        assert_eq!(e.code, "LTHOUSE_ITEM");
        assert!(e.message.contains("jukebox"));
    }
}
