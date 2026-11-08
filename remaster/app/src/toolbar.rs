//! The window's toolbar (LT-117): the look, the lights, the aspect, the frame rate and the draw
//! distance, the picture beside the native view, and the debug stats. Drawn with egui in Living
//! Tool's own colours (`app/src/styles/tokens.css`), light or dark as the system is; the game's
//! picture under it is always on black.
//!
//! A [`View`] is everything the toolbar sets. It is saved to `--settings` (plain `key=value`
//! lines) whenever it changes, so the window opens as the player left it.

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText, Stroke, TextStyle, Vec2};
use lt_native::light::Lights;

/// The aspects the toolbar and the A key step through: a name, a width over height, or `None`
/// for the window's own shape.
pub const ASPECTS: [(&str, Option<f64>); 5] = [("4:3", Some(4.0 / 3.0)), ("16:10", Some(1.6)), ("16:9", Some(16.0 / 9.0)), ("21:9", Some(21.0 / 9.0)), ("Window", None)];
pub const FRAME_RATES: [u32; 3] = [30, 60, 120];

/// The toolbar's height, in points.
pub const HEIGHT: f32 = 40.0;

/// What the window shows and how.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// Lit (LT-112), else the game's original look.
    pub lit: bool,
    pub lights: Lights,
    /// An index into [`ASPECTS`].
    pub aspect: usize,
    pub fps: u32,
    pub distance: f64,
    /// The native view alone, else the emulated picture beside it.
    pub native_only: bool,
    /// The debug stats over the picture (F3).
    pub stats: bool,
    /// The toolbar itself (F2 hides and shows it).
    pub toolbar: bool,
}

impl View {
    /// `key=value` lines.
    pub fn to_text(&self) -> String {
        let on = |b: bool| if b { "on" } else { "off" };
        format!(
            "look={}\nshadows={}\nlamps={}\naspect={}\nfps={}\ndistance={}\nview={}\nstats={}\ntoolbar={}\n",
            if self.lit { "lit" } else { "original" },
            on(self.lights.shadows),
            on(self.lights.lamps),
            ASPECTS[self.aspect].0,
            self.fps,
            self.distance,
            if self.native_only { "native" } else { "both" },
            on(self.stats),
            on(self.toolbar)
        )
    }

    /// `self` with what `text` (from [`View::to_text`]) sets; lines it doesn't know are skipped.
    pub fn with_text(mut self, text: &str) -> View {
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let on = v == "on";
            match k {
                "look" => self.lit = v != "original",
                "shadows" => self.lights.shadows = on,
                "lamps" => self.lights.lamps = on,
                "aspect" => {
                    if let Some(i) = ASPECTS.iter().position(|(n, _)| n.eq_ignore_ascii_case(v)) {
                        self.aspect = i;
                    }
                }
                "fps" => {
                    if let Some(f) = v.parse().ok().filter(|f| FRAME_RATES.contains(f)) {
                        self.fps = f;
                    }
                }
                "distance" => {
                    if let Some(d) = v.parse().ok().filter(|d: &f64| (1.0..=lt_native::wide::MAX_DISTANCE).contains(d)) {
                        self.distance = d;
                    }
                }
                "view" => self.native_only = v != "both",
                "stats" => self.stats = on,
                "toolbar" => self.toolbar = on,
                _ => {}
            }
        }
        self
    }
}

/// Living Tool's colours for one theme.
struct Palette {
    bar: Color32,
    track: Color32,
    raised: Color32,
    border: Color32,
    text: Color32,
    muted: Color32,
    subtle: Color32,
    /// Something switched on: the ice tint and the primary as text.
    on_fill: Color32,
    on_text: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bar: hex(0x111113),
            track: hex(0x222226),
            raised: hex(0x2C2C31),
            border: hex(0x2A2A2F),
            text: hex(0xEDEDEF),
            muted: hex(0xA1A1AA),
            subtle: hex(0x7C7C86),
            on_fill: hex(0x0F2A4D),
            on_text: hex(0x3D95FF),
        }
    } else {
        Palette {
            bar: hex(0xFAFAFA),
            track: hex(0xF4F4F5),
            raised: hex(0xFFFFFF),
            border: hex(0xE7E7EA),
            text: hex(0x18181B),
            muted: hex(0x5C5C66),
            subtle: hex(0x8E8E98),
            on_fill: hex(0xEAF3FF),
            on_text: hex(0x066DEB),
        }
    }
}

/// Inter for everything, as in Living Tool.
pub fn fonts(ctx: &egui::Context) {
    let mut f = FontDefinitions::default();
    f.font_data.insert("Inter".into(), FontData::from_static(include_bytes!("../fonts/Inter-Regular.ttf")).into());
    f.font_data.insert("Inter Medium".into(), FontData::from_static(include_bytes!("../fonts/Inter-Medium.ttf")).into());
    f.families.get_mut(&FontFamily::Proportional).expect("proportional").insert(0, "Inter".into());
    f.families.insert(FontFamily::Name("medium".into()), vec!["Inter Medium".into(), "Inter".into()]);
    ctx.set_fonts(f);
    ctx.style_mut(|s| {
        s.text_styles.insert(TextStyle::Body, FontId::new(13.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(12.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Small, FontId::new(11.0, FontFamily::Proportional));
    });
}

/// One choice in a segmented group: the selected segment is raised out of the track.
fn segment(ui: &mut egui::Ui, p: &Palette, label: &str, selected: bool, enabled: bool) -> bool {
    let text = RichText::new(label).size(12.0).color(if !enabled {
        p.subtle
    } else if selected {
        p.text
    } else {
        p.muted
    });
    let text = if selected { text.family(FontFamily::Name("medium".into())) } else { text };
    let b = egui::Button::new(text)
        .fill(if selected && enabled { p.raised } else { Color32::TRANSPARENT })
        .stroke(if selected && enabled { Stroke::new(1.0_f32, p.border) } else { Stroke::NONE })
        .corner_radius(CornerRadius::same(5))
        .min_size(Vec2::new(0.0, 22.0));
    ui.add_enabled(enabled, b).clicked() && !selected
}

/// A group of segments on a muted track, labelled. Returns the one clicked.
fn group<T: Copy + PartialEq>(ui: &mut egui::Ui, p: &Palette, label: &str, items: &[(String, T)], current: T, enabled: bool) -> Option<T> {
    ui.label(RichText::new(label).size(12.0).color(p.muted));
    let mut picked = None;
    egui::Frame::new().fill(p.track).corner_radius(CornerRadius::same(7)).inner_margin(Margin::same(2)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.horizontal(|ui| {
            for (name, v) in items {
                if segment(ui, p, name, *v == current, enabled) {
                    picked = Some(*v);
                }
            }
        });
    });
    picked
}

/// A toggle: on is the ice tint with primary text (Living Tool's "switched on").
fn toggle(ui: &mut egui::Ui, p: &Palette, label: &str, on: bool, enabled: bool) -> bool {
    let colour = if !enabled {
        p.subtle
    } else if on {
        p.on_text
    } else {
        p.muted
    };
    let b = egui::Button::new(RichText::new(label).size(12.0).color(colour))
        .fill(if on && enabled { p.on_fill } else { Color32::TRANSPARENT })
        .stroke(Stroke::new(1.0_f32, if on && enabled { p.on_fill } else { p.border }))
        .corner_radius(CornerRadius::same(6))
        .min_size(Vec2::new(0.0, 26.0));
    ui.add_enabled(enabled, b).clicked()
}

/// Draws the toolbar into `ctx`, changing `view` as it's clicked. Returns its height in points
/// (0 when hidden).
pub fn show(ctx: &egui::Context, dark: bool, view: &mut View) -> f32 {
    if !view.toolbar {
        return 0.0;
    }
    let p = palette(dark);
    let frame = egui::Frame::new().fill(p.bar).inner_margin(Margin::symmetric(12, 0)).stroke(Stroke::new(1.0_f32, p.border));
    let r = egui::TopBottomPanel::top("toolbar").exact_height(HEIGHT).frame(frame).show_separator_line(false).show(ctx, |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new("Remastered").size(13.0).family(FontFamily::Name("medium".into())).color(p.text));
            ui.add_space(8.0);
            let look = [("Lit".to_string(), true), ("Original".to_string(), false)];
            if let Some(v) = group(ui, &p, "Look", &look, view.lit, true) {
                view.lit = v;
            }
            ui.add_space(4.0);
            ui.label(RichText::new("Lights").size(12.0).color(p.muted));
            if toggle(ui, &p, "Shadows", view.lights.shadows, view.lit) {
                view.lights.shadows = !view.lights.shadows;
            }
            if toggle(ui, &p, "Lamps", view.lights.lamps, view.lit) {
                view.lights.lamps = !view.lights.lamps;
            }
            ui.add_space(4.0);
            let aspects: Vec<(String, usize)> = ASPECTS.iter().enumerate().map(|(i, (n, _))| (n.to_string(), i)).collect();
            if let Some(v) = group(ui, &p, "Aspect", &aspects, view.aspect, true) {
                view.aspect = v;
            }
            ui.add_space(4.0);
            let rates: Vec<(String, u32)> = FRAME_RATES.iter().map(|&f| (f.to_string(), f)).collect();
            if let Some(v) = group(ui, &p, "Frame rate", &rates, view.fps, true) {
                view.fps = v;
            }
            ui.add_space(4.0);
            let distances: Vec<(String, u32)> = (1..=lt_native::wide::MAX_DISTANCE as u32).map(|d| (format!("{d}×"), d)).collect();
            if let Some(v) = group(ui, &p, "Distance", &distances, view.distance as u32, true) {
                view.distance = f64::from(v);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if toggle(ui, &p, "Stats", view.stats, true) {
                    view.stats = !view.stats;
                }
                if toggle(ui, &p, "Original beside", !view.native_only, true) {
                    view.native_only = !view.native_only;
                }
            });
        });
    });
    r.response.rect.height()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standard() -> View {
        View { lit: true, lights: Lights::ALL, aspect: 4, fps: 60, distance: 1.0, native_only: true, stats: false, toolbar: true }
    }

    #[test]
    fn a_view_saves_and_reads_back() {
        let v = View { lit: false, lights: Lights { shadows: false, lamps: true }, aspect: 2, fps: 120, distance: 3.0, native_only: false, stats: true, toolbar: false };
        assert_eq!(standard().with_text(&v.to_text()), v);
        assert!(v.to_text().contains("aspect=16:9\nfps=120\n"));
    }

    #[test]
    fn unknown_or_bad_lines_keep_what_was_there() {
        let v = standard().with_text("fps=75\naspect=5:4\nwhat=ever\nnot a line\ndistance=9");
        assert_eq!(v, standard());
    }
}
