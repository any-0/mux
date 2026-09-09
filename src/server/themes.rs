//! The theme picker: what is installed, and what each one looks like.

use std::{fs, path::Path};

use anyhow::{Context, Result};

use crate::{
    config::{Palette, Theme},
    frame::{CellAttributes, Frame, Rgb},
};

/// What a theme calls the file that colours mux.
pub(super) const THEME_FILE: &str = "mux.toml";

/// The link, beside the theme directory, that names the theme in use.
pub(super) const THEME_CURRENT_LINK: &str = "current";

/// An open theme picker.
///
/// Every theme is loaded when the picker opens, so moving the selection repaints
/// the whole screen in the highlighted theme without touching the disk again.
pub(super) struct ThemePicker {
    pub(super) entries: Vec<ThemeEntry>,
    pub(super) selected: usize,
    /// Where the theme in use sits in `entries`, when it is one of them.
    pub(super) in_use: Option<usize>,
}

pub(super) struct ThemeEntry {
    pub(super) name: String,
    pub(super) theme: Theme,
}

impl ThemePicker {
    pub(super) fn theme(&self) -> Theme {
        self.entries[self.selected].theme
    }

    pub(super) fn name(&self) -> &str {
        &self.entries[self.selected].name
    }
}

/// Reads every theme under `directory`, which holds one directory per theme.
///
/// A theme is anything with a readable `mux.toml`; a directory without one, or
/// with one mux cannot parse, is simply not offered rather than failing the
/// whole picker.
pub(super) fn scan_themes(directory: &Path) -> Result<Vec<ThemeEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read theme directory {}", directory.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path().join(THEME_FILE);
        // Themes are usually symlinks into the Nix store, so the file type has
        // to be followed rather than taken from the directory entry.
        if !path.is_file() {
            continue;
        }
        if let Ok(theme) = Theme::load(&path) {
            entries.push(ThemeEntry { name, theme });
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

/// The name of the theme in use, read from the `current` link the `theme`
/// command maintains beside the theme directory.
pub(super) fn current_theme_name(directory: &Path) -> Option<String> {
    let link = directory.parent()?.join(THEME_CURRENT_LINK);
    let target = fs::read_link(&link).ok()?;
    Some(target.file_name()?.to_string_lossy().into_owned())
}

/// Width the card wants, which is what the swatches need.
const THEME_SHOWCASE_WIDTH: u16 = 60;

/// Every colour defined by `dotfiles/themes/palettes.nix`, in source order and
/// with the names used there.
fn palette_colors(palette: &Palette) -> [(&'static str, Rgb); 14] {
    [
        ("background", palette.background),
        ("foreground", palette.foreground),
        ("surface", palette.surface),
        ("surfaceRaised", palette.surface_raised),
        ("muted", palette.muted),
        ("accent", palette.accent),
        ("secondary", palette.secondary),
        ("success", palette.success),
        ("warning", palette.warning),
        ("danger", palette.danger),
        ("selection", palette.selection),
        ("diffAdd", palette.diff_add),
        ("diffDelete", palette.diff_delete),
        ("diffChange", palette.diff_change),
    ]
}

/// Rows a run of swatches takes once it has wrapped, each `padding` cells wider
/// than its name and `gap` cells apart.
fn swatch_rows(names: &[(&str, Rgb)], padding: u16, gap: u16, width: u16) -> u16 {
    let mut rows = 1;
    let mut used = 0;
    for (name, _) in names {
        let needed = name.chars().count() as u16 + padding;
        if used + needed > width && used > 0 {
            rows += 1;
            used = 0;
        }
        used += needed + gap;
    }
    rows
}

/// Rows the palette needs at this width.
fn theme_palette_rows(palette: &Palette, width: u16) -> u16 {
    swatch_rows(&palette_colors(palette), 2, 1, width)
}

/// Shows the palette the highlighted theme is made of.
fn render_theme_palette(frame: &mut Frame, top: u16, col: u16, width: u16, theme: &Theme) {
    let palette = theme.palette;
    let mut row = top;
    let mut at = col;
    for (name, color) in palette_colors(&palette) {
        let tag = format!(" {name} ");
        let needed = tag.chars().count() as u16;
        if at + needed > col + width && at > col {
            row += 1;
            at = col;
        }
        at = frame.set_text(
            row,
            at,
            &tag,
            CellAttributes::colors(
                contrasting_shade(color, palette.foreground, palette.background),
                color,
            ),
        ) + 1;
    }
}

/// Whichever of two shades `color` stands out against best.
///
/// This picks the text to write on a swatch: a theme is free to use the exact
/// colour of the surface its swatch lands on, and the swatch still has to be
/// legible.
pub(super) fn contrasting_shade(color: Rgb, first: Rgb, second: Rgb) -> Rgb {
    if luminance_gap(color, first) >= luminance_gap(color, second) {
        first
    } else {
        second
    }
}

fn luminance_gap(left: Rgb, right: Rgb) -> u32 {
    luminance(left).abs_diff(luminance(right))
}

/// Perceived brightness, scaled by 10,000 to stay in integers.
fn luminance((red, green, blue): Rgb) -> u32 {
    2126 * red as u32 + 7152 * green as u32 + 722 * blue as u32
}

/// Where the picker's card sits: its top-left corner and size, in one-based
/// screen coordinates.
pub(super) struct ThemeCard {
    pub(super) top: u16,
    pub(super) left: u16,
    pub(super) rows: u16,
    pub(super) cols: u16,
    /// Which themes go on each row of the strip along the top.
    pub(super) tabs: Vec<Vec<usize>>,
}

/// Cells one theme's tab takes: a marker, its shortcut, its name, and the three
/// colours it is offering.
fn theme_tab_width(name: &str) -> u16 {
    name.chars().count() as u16 + 9
}

/// Wraps the tabs across as many rows as they need.
pub(super) fn theme_tab_rows(names: &[&str], width: u16) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 0;
    for (index, name) in names.iter().enumerate() {
        let tab = theme_tab_width(name);
        if used + tab > width && !rows.last().unwrap().is_empty() {
            rows.push(Vec::new());
            used = 0;
        }
        rows.last_mut().unwrap().push(index);
        used += tab;
    }
    rows
}

/// Sizes the card to what it has to show, then to what the terminal has.
///
/// The picker is a dialog rather than a screen: it is only as big as the themes
/// and the preview need, so nothing in it floats in empty space, and the panes
/// it covers stay visible around it.
pub(super) fn theme_card(rows: u16, cols: u16, names: &[&str], palette: &Palette) -> ThemeCard {
    let inside = THEME_SHOWCASE_WIDTH.min(cols.saturating_sub(6)).max(1);
    let tabs = theme_tab_rows(names, inside);
    let wanted = tabs.len() as u16 + 1 + theme_palette_rows(palette, inside);
    let inside_rows = wanted.min(rows.saturating_sub(2)).max(1);
    let card_rows = inside_rows + 2;
    let card_cols = inside + 4;
    ThemeCard {
        top: rows.saturating_sub(card_rows) / 2 + 1,
        left: cols.saturating_sub(card_cols) / 2 + 1,
        rows: card_rows,
        cols: card_cols,
        tabs,
    }
}

/// Draws the theme picker over the screen it was opened from: the installed
/// themes along the top of a card, and what the highlighted one looks like
/// underneath.
///
/// Every colour in the card comes from the highlighted theme rather than the
/// one in use, so moving along the strip is itself the preview.
pub(super) fn render_theme_picker(picker: &ThemePicker, frame: &mut Frame, rows: u16, cols: u16) {
    let theme = picker.theme();
    let names: Vec<_> = picker
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    let card = theme_card(rows, cols, &names, &theme.palette);
    let chrome = CellAttributes::colors(theme.panel_foreground, theme.panel_background);
    let border = CellAttributes::colors(theme.divider, theme.panel_background);
    for row in card.top..card.top + card.rows {
        frame.fill(row, card.left, card.cols, chrome);
    }

    let inner = card.cols.saturating_sub(2) as usize;
    let right = card.left + card.cols - 1;
    let bottom = card.top + card.rows - 1;
    frame.set_text(
        card.top,
        card.left,
        &format!("╭{}╮", "─".repeat(inner)),
        border,
    );
    frame.set_text(
        bottom,
        card.left,
        &format!("╰{}╯", "─".repeat(inner)),
        border,
    );
    for row in card.top + 1..bottom {
        frame.set_cell(row, card.left, "│", border);
        frame.set_cell(row, right, "│", border);
    }
    let heading = CellAttributes::colors(theme.panel_heading, theme.panel_background);
    if inner > 12 {
        let title = format!(
            " {} theme{} ",
            picker.entries.len(),
            if picker.entries.len() == 1 { "" } else { "s" }
        );
        frame.set_text(card.top, card.left + 2, &title, heading.bold());
        let hint = " ←→ browse · enter apply · esc cancel ";
        if inner > hint.chars().count() + 4 {
            frame.set_text(bottom, card.left + 3, hint, heading.dim());
        }
    }

    let (top, left) = (card.top + 1, card.left + 2);
    let width = card.cols - 4;
    let strip = render_theme_tabs(picker, frame, &card.tabs, top, left, width);
    if card.rows.saturating_sub(2) <= strip {
        return;
    }
    frame.set_text(
        top + strip,
        card.left,
        &format!("├{}┤", "─".repeat(inner)),
        border,
    );
    render_theme_palette(frame, top + strip + 1, left, width, &theme);
}

/// The strip of themes along the top of the card, returning the rows it used.
///
/// Each tab carries a few of its own theme's colours, so the strip shows what
/// it is offering without having to be walked through one by one.
fn render_theme_tabs(
    picker: &ThemePicker,
    frame: &mut Frame,
    tabs: &[Vec<usize>],
    top: u16,
    left: u16,
    width: u16,
) -> u16 {
    let theme = picker.theme();
    for (offset, indices) in tabs.iter().enumerate() {
        let row = top + offset as u16;
        let mut at = left;
        for index in indices {
            let entry = &picker.entries[*index];
            let selected = *index == picker.selected;
            let background = if selected {
                theme.panel_selected
            } else {
                theme.panel_background
            };
            let attributes = CellAttributes::colors(
                if selected {
                    theme.panel_foreground
                } else {
                    theme.panel_row_foreground
                },
                background,
            );
            let tab = theme_tab_width(&entry.name);
            if at + tab > left + width {
                break;
            }
            frame.fill(row, at, tab, attributes);
            // The dot marks the theme in use, which is not always the one being
            // previewed.
            if picker.in_use == Some(*index) {
                frame.set_cell(
                    row,
                    at,
                    "●",
                    CellAttributes::colors(theme.popup_accent, background),
                );
            }
            let shortcut = match index {
                index if *index < 9 => (index + 1).to_string(),
                _ => " ".into(),
            };
            let after = frame.set_text(
                row,
                at + 1,
                &format!(" {shortcut} {} ", entry.name),
                if selected {
                    attributes.bold()
                } else {
                    attributes
                },
            );
            for (chip, color) in [
                entry.theme.bar_active,
                entry.theme.popup_accent,
                entry.theme.bell_base,
            ]
            .into_iter()
            .enumerate()
            {
                frame.set_cell(
                    row,
                    after + chip as u16,
                    "█",
                    CellAttributes::colors(color, background),
                );
            }
            at += tab;
        }
    }
    tabs.len() as u16
}
