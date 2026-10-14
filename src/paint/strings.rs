//! i18n scaffold: a single lookup point (`t`) that every user-facing Paint
//! string should eventually route through, keyed by a stable string ID
//! rather than written inline — so translations could be added later by
//! extending `TABLE`, without touching call sites.
//!
//! This is intentionally English-only. Fabricating translations into other
//! languages without real translators would be actively wrong (silently
//! wrong UI text is worse than none), so `TABLE` holds only the source
//! strings; `t()` falls back to the key itself if one is ever missing,
//! rather than panicking.
//!
//! Scope note: only `Tool::label()`/`strip_label()` are routed through
//! this so far (the ~35 core tool-name strings) — this is a scaffold, not
//! a completed sweep of every string in the module. Status messages and
//! legend labels elsewhere in `paint.rs` are the natural next candidates
//! whenever localization becomes a real, funded effort with actual
//! translators.

pub fn t(key: &'static str) -> &'static str {
    match TABLE.iter().find(|(k, _)| *k == key) {
        Some((_, v)) => v,
        None => key,
    }
}

const TABLE: &[(&str, &str)] = &[
    ("tool.pencil.label", "Pencil"),
    ("tool.pencil.strip", "Pencil"),
    ("tool.brush.label", "Brush"),
    ("tool.brush.strip", "Brush"),
    ("tool.eraser.label", "Eraser"),
    ("tool.eraser.strip", "Erase"),
    ("tool.color_eraser.label", "Color Eraser"),
    ("tool.color_eraser.strip", "ColorEr"),
    ("tool.airbrush.label", "Airbrush"),
    ("tool.airbrush.strip", "Spray"),
    ("tool.fill.label", "Fill"),
    ("tool.fill.strip", "Fill"),
    ("tool.picker.label", "Pick Color"),
    ("tool.picker.strip", "Pick"),
    ("tool.line.label", "Line"),
    ("tool.line.strip", "Line"),
    ("tool.rect.label", "Rectangle"),
    ("tool.rect.strip", "Rect"),
    ("tool.round_rect.label", "Rounded Rectangle"),
    ("tool.round_rect.strip", "RRect"),
    ("tool.ellipse.label", "Ellipse"),
    ("tool.ellipse.strip", "Oval"),
    ("tool.curve.label", "Curve"),
    ("tool.curve.strip", "Curve"),
    ("tool.polygon.label", "Polygon"),
    ("tool.polygon.strip", "Poly"),
    ("tool.select_rect.label", "Select"),
    ("tool.select_rect.strip", "Select"),
    ("tool.select_free.label", "Free-Form Select"),
    ("tool.select_free.strip", "Lasso"),
    ("tool.text.label", "Text"),
    ("tool.text.strip", "Text"),
    ("tool.zoom.label", "Magnifier"),
    ("tool.zoom.strip", "Zoom"),
];
