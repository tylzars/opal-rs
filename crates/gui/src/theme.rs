//! Colours, fonts and the custom widgets of the control panel.

use std::ops::RangeInclusive;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontFamily, FontId, Margin, Rect, Response, Sense, Shadow, Stroke, StrokeKind,
    TextStyle, Ui, Vec2, pos2, vec2,
};

pub const BG: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x12);
pub const PANEL: Color32 = Color32::from_rgb(0x14, 0x15, 0x19);
pub const CARD: Color32 = Color32::from_rgb(0x1b, 0x1d, 0x22);
pub const RAISED: Color32 = Color32::from_rgb(0x26, 0x29, 0x30);
pub const HOVER: Color32 = Color32::from_rgb(0x30, 0x33, 0x3b);
pub const LINE: Color32 = Color32::from_rgb(0x2a, 0x2d, 0x34);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe9, 0xed);
pub const WEAK: Color32 = Color32::from_rgb(0x8d, 0x91, 0x9b);
pub const ACCENT: Color32 = Color32::from_rgb(0x6c, 0x8c, 0xff);
pub const GREEN: Color32 = Color32::from_rgb(0x3c, 0xd0, 0x7a);
pub const RED: Color32 = Color32::from_rgb(0xff, 0x5d, 0x62);
pub const AMBER: Color32 = Color32::from_rgb(0xf2, 0xb3, 0x4a);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        use FontFamily::Proportional;
        s.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, Proportional)),
            (TextStyle::Body, FontId::new(14.5, Proportional)),
            (TextStyle::Button, FontId::new(14.0, Proportional)),
            (TextStyle::Small, FontId::new(12.5, Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(14.0, 7.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.scroll.bar_width = 6.0;
        s.spacing.scroll.floating = true;

        let v = &mut s.visuals;
        v.dark_mode = true;
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.window_stroke = Stroke::new(1.0, LINE);
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(10);
        v.window_shadow = Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(120),
        };
        v.popup_shadow = v.window_shadow;
        v.extreme_bg_color = Color32::from_rgb(0x11, 0x12, 0x15);
        v.faint_bg_color = CARD;
        v.weak_text_color = Some(WEAK);
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT;
        v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        v.error_fg_color = RED;
        v.warn_fg_color = AMBER;
        let radius = CornerRadius::same(10);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CARD;
        w.noninteractive.weak_bg_fill = CARD;
        w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = radius;
        for (state, fill) in [
            (&mut w.inactive, RAISED),
            (&mut w.hovered, HOVER),
            (&mut w.active, HOVER),
            (&mut w.open, RAISED),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::NONE;
            state.fg_stroke = Stroke::new(1.5, TEXT);
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
        w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x3c, 0x40, 0x4a));
    });
}

/// A rounded section with an optional title.
pub fn card<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(CARD)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(14)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                section(ui, title);
            }
            add(ui)
        })
        .inner
}

/// Small capitals heading a group of settings.
pub fn section(ui: &mut Ui, title: &str) {
    let text = egui::RichText::new(title.to_uppercase())
        .size(12.0)
        .strong()
        .color(WEAK);
    ui.label(text.extra_letter_spacing(0.8));
}

/// Small grey text that wraps.
pub fn hint(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).small().color(WEAK)).wrap());
}

/// Paints an on/off switch in `rect`, `t` going from 0 (off) to 1 (on).
fn paint_switch(ui: &Ui, rect: Rect, t: f32, hovered: bool) {
    let p = ui.painter();
    let off = if hovered { HOVER } else { RAISED };
    let fill = lerp_color(off, ACCENT, t);
    p.rect_filled(rect, CornerRadius::same(255), fill);
    let r = rect.height() / 2.0 - 3.0;
    let x = egui::lerp((rect.left() + r + 3.0)..=(rect.right() - r - 3.0), t);
    let c = pos2(x, rect.center().y);
    p.circle_filled(c + vec2(0.0, 1.0), r, Color32::from_black_alpha(60));
    p.circle_filled(c, r, Color32::WHITE);
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| egui::lerp(x as f32..=y as f32, t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// A row with a title, an optional description and a switch on the right.
/// The whole row is clickable. Returns true when flipped.
pub fn switch_row(ui: &mut Ui, title: &str, desc: &str, on: &mut bool) -> bool {
    let size = vec2(44.0, 24.0);
    let width = ui.available_width();
    let top = ui.cursor().min;
    let text = ui
        .allocate_ui_with_layout(
            vec2(width - size.x - 16.0, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(egui::RichText::new(title).size(15.5).color(TEXT));
                if !desc.is_empty() {
                    hint(ui, desc);
                }
            },
        )
        .response
        .rect;
    let height = text.height().max(size.y);
    let row = Rect::from_min_size(top, vec2(width, height));
    let switch = Rect::from_center_size(pos2(row.right() - size.x / 2.0, row.center().y), size);
    ui.expand_to_include_rect(row);
    let resp = ui.interact(row, ui.id().with(("switch", title)), Sense::click());
    let clicked = resp.clicked();
    if clicked {
        *on = !*on;
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    paint_switch(ui, switch, t, resp.hovered());
    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    clicked
}

/// Numbers a slider can drive.
pub trait Num: Copy + PartialEq {
    fn to_f64(self) -> f64;
    fn from_f64(v: f64) -> Self;
}

impl Num for i32 {
    fn to_f64(self) -> f64 {
        self as f64
    }
    fn from_f64(v: f64) -> Self {
        v.round() as i32
    }
}

impl Num for f32 {
    fn to_f64(self) -> f64 {
        self as f64
    }
    fn from_f64(v: f64) -> Self {
        v as f32
    }
}

/// A labelled full width slider with the value shown on the right.
/// `step` rounds float values; `log` spaces the range logarithmically.
pub struct Slider<'a, N: Num> {
    label: &'a str,
    value: &'a mut N,
    range: RangeInclusive<N>,
    text: String,
    log: bool,
    step: f64,
}

impl<'a, N: Num> Slider<'a, N> {
    pub fn new(label: &'a str, value: &'a mut N, range: RangeInclusive<N>, text: String) -> Self {
        Self {
            label,
            value,
            range,
            text,
            log: false,
            step: 0.01,
        }
    }

    pub fn log(mut self) -> Self {
        self.log = true;
        self
    }

    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let (lo, hi) = (self.range.start().to_f64(), self.range.end().to_f64());
        let to_t = |v: f64| {
            if self.log {
                (v.max(lo).ln() - lo.ln()) / (hi.ln() - lo.ln())
            } else {
                (v - lo) / (hi - lo)
            }
        };
        let from_t = |t: f64| {
            if self.log {
                (lo.ln() + t * (hi.ln() - lo.ln())).exp()
            } else {
                lo + t * (hi - lo)
            }
        };

        let top = ui.cursor().min;
        let width = ui.available_width();
        let label = ui
            .painter()
            .layout_no_wrap(self.label.to_owned(), FontId::proportional(14.5), TEXT);
        let value = ui.painter().layout_no_wrap(self.text, FontId::proportional(14.0), WEAK);
        let label_h = label.size().y;
        ui.painter().galley(top, label, TEXT);
        ui.painter()
            .galley(pos2(top.x + width - value.size().x, top.y), value, WEAK);
        ui.allocate_exact_size(vec2(width, label_h), Sense::hover());
        ui.add_space(4.0 - ui.spacing().item_spacing.y);
        let (rect, mut resp) = ui.allocate_exact_size(vec2(width, 22.0), Sense::click_and_drag());
        let knob_r = 9.0;
        let rail = Rect::from_min_max(
            pos2(rect.left() + knob_r, rect.center().y - 2.5),
            pos2(rect.right() - knob_r, rect.center().y + 2.5),
        );
        if let Some(p) = resp.interact_pointer_pos()
            && (resp.dragged() || resp.clicked())
        {
            let t = ((p.x - rail.left()) / rail.width()).clamp(0.0, 1.0) as f64;
            let mut v = from_t(t);
            if self.step > 0.0 {
                v = (v / self.step).round() * self.step;
            }
            let v = N::from_f64(v.clamp(lo, hi));
            if v != *self.value {
                *self.value = v;
                resp.mark_changed();
            }
        }
        let t = to_t(self.value.to_f64()).clamp(0.0, 1.0) as f32;
        let x = egui::lerp(rail.left()..=rail.right(), t);
        let p = ui.painter();
        p.rect_filled(rail, CornerRadius::same(255), RAISED);
        p.rect_filled(
            Rect::from_min_max(rail.min, pos2(x, rail.max.y)),
            CornerRadius::same(255),
            ACCENT,
        );
        let active = resp.hovered() || resp.dragged();
        let c = pos2(x, rail.center().y);
        let r = if active { knob_r } else { knob_r - 1.0 };
        if active {
            p.circle_filled(c, r + 5.0, ACCENT.gamma_multiply(0.25));
        }
        p.circle_filled(c + vec2(0.0, 1.0), r, Color32::from_black_alpha(70));
        p.circle_filled(c, r, Color32::WHITE);
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    }
}

/// A row of equal width choices; returns true when the choice changes.
pub fn segmented<T: PartialEq + Clone>(ui: &mut Ui, id: &str, current: &mut T, options: &[(T, &str)]) -> bool {
    let height = 34.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let p = ui.painter().clone();
    p.rect(
        rect,
        CornerRadius::same(10),
        Color32::from_rgb(0x10, 0x11, 0x14),
        Stroke::new(1.0, LINE),
        StrokeKind::Inside,
    );
    let inner = rect.shrink(3.0);
    let w = inner.width() / options.len() as f32;
    let mut changed = false;
    for (i, (value, label)) in options.iter().enumerate() {
        let r = Rect::from_min_size(pos2(inner.left() + w * i as f32, inner.top()), vec2(w, inner.height()));
        let resp = ui.interact(r, ui.id().with((id, i)), Sense::click());
        if resp.clicked() && *current != *value {
            *current = value.clone();
            changed = true;
        }
        let selected = *current == *value;
        let t = ui.ctx().animate_bool_responsive(resp.id, selected);
        if t > 0.0 {
            p.rect_filled(r, CornerRadius::same(8), RAISED.gamma_multiply(t));
        } else if resp.hovered() {
            p.rect_filled(r, CornerRadius::same(8), Color32::from_rgb(0x1c, 0x1e, 0x23));
        }
        let color = if selected { TEXT } else { WEAK };
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(13.5),
            color,
        );
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    changed
}

/// A filled accent button.
pub fn primary(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(Color32::WHITE).strong())
            .fill(ACCENT)
            .corner_radius(10),
    )
}

/// A rounded label with a coloured dot, painted at `pos` anchored by `anchor`.
pub fn pill(ui: &Ui, pos: egui::Pos2, anchor: Align2, dot: Option<Color32>, text: &str) -> Rect {
    let font = FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, TEXT);
    let dot_w = if dot.is_some() { 14.0 } else { 0.0 };
    let size = vec2(galley.size().x + dot_w + 22.0, 26.0);
    let rect = anchor.anchor_size(pos, size);
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(255),
        Color32::from_black_alpha(150),
        Stroke::new(1.0, Color32::from_white_alpha(18)),
        StrokeKind::Inside,
    );
    if let Some(c) = dot {
        p.circle_filled(pos2(rect.left() + 15.0, rect.center().y), 4.0, c);
    }
    p.galley(
        pos2(rect.left() + 11.0 + dot_w, rect.center().y - galley.size().y / 2.0),
        galley,
        TEXT,
    );
    rect
}

/// Size of the largest 16:9 box that fits in `avail`.
pub fn fit_16_9(avail: Vec2) -> Vec2 {
    let w = avail.x.min(avail.y * 16.0 / 9.0).max(0.0);
    vec2(w, w * 9.0 / 16.0)
}
