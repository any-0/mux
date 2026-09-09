//! A pending bell, and the animation that shows one.

use std::time::Instant;

use crate::{
    config::{BellStyle, Theme},
    frame::{CellAttributes, Frame, Rgb},
};

pub(super) const BELL_SHIMMER_MICROS: u128 = 1_440_000;

pub(super) const BELL_BREAK_MICROS: u128 = 960_000;

/// How far the bell highlight reaches either side of its centre, in cells.
const BELL_HIGHLIGHT_SPAN: i64 = 3;

/// How long the bell colour takes to slide in over a label, and later out.
const BELL_SLIDE_MICROS: u128 = 260_000;

/// Sub-cell resolution of the highlight position, so it glides between cells
/// instead of snapping from one to the next.
const BELL_SUBCELL: i64 = 256;

pub(super) struct BellState {
    /// When the bell first appeared, which drives the slide-in. Kept across
    /// later rings so a repeat does not slide the colour in a second time.
    pub(super) appeared: Instant,
    pub(super) started: Instant,
    pub(super) render_token: u64,
    pub(super) count: usize,
    pub(super) repeat: bool,
    pub(super) pane_id: usize,
}

/// Answers a pending bell on the window being looked at. With a shimmer to
/// play it runs one last pass and then drops; with nothing to animate there is
/// nothing to wait for, so the bell goes at once.
pub(super) fn play_bell_once(bell: &mut Option<BellState>, shimmer: bool) {
    let Some(state) = bell else {
        return;
    };
    if !shimmer {
        *bell = None;
        return;
    }
    if state.repeat {
        state.started = Instant::now();
        state.render_token = 0;
        state.repeat = false;
    }
}

#[derive(Clone, Copy)]
pub(super) struct BellVisual {
    /// Position within the sweep, or `None` while the label rests between passes.
    pub(super) shimmer: Option<u128>,
    /// How much of the label the bell colour covers as it moves in or out.
    pub(super) slide: BellSlide,
}

#[derive(Clone, Copy)]
pub(super) enum BellSlide {
    /// Moving in from the left edge; `0` covers none of the label, `255` all of it.
    In(u16),
    Covered,
    /// Moving out past the right edge; `255` still covers the label, `0` none.
    Out(u16),
}

/// How a pending bell looks to a client right now, or `None` when that client
/// asked not to see one.
pub(super) fn bell_visual(bell: &BellState, style: BellStyle) -> Option<BellVisual> {
    match style {
        BellStyle::Shimmer => Some(shimmering_bell_visual(bell)),
        // The resting state of the sweep: covered, at its bright end, still.
        BellStyle::Steady => Some(BellVisual {
            shimmer: None,
            slide: BellSlide::Covered,
        }),
        BellStyle::None => None,
    }
}

fn shimmering_bell_visual(bell: &BellState) -> BellVisual {
    let elapsed = bell.started.elapsed().as_micros();
    let cycle_elapsed = if bell.repeat {
        elapsed % (BELL_SHIMMER_MICROS + BELL_BREAK_MICROS)
    } else {
        elapsed.min(BELL_SHIMMER_MICROS.saturating_sub(1))
    };
    let shimmer = (cycle_elapsed < BELL_SHIMMER_MICROS).then_some(cycle_elapsed);
    // A bell that will not repeat is dropped once its pass ends, so it spends
    // the tail of that pass moving back out again.
    let remaining = BELL_SHIMMER_MICROS.saturating_sub(elapsed);
    let appeared = bell.appeared.elapsed().as_micros();
    let slide = if appeared < BELL_SLIDE_MICROS {
        BellSlide::In(slide_progress(appeared))
    } else if !bell.repeat && remaining < BELL_SLIDE_MICROS {
        BellSlide::Out(slide_progress(remaining))
    } else {
        BellSlide::Covered
    };
    BellVisual { shimmer, slide }
}

fn slide_progress(elapsed: u128) -> u16 {
    (elapsed.min(BELL_SLIDE_MICROS) * 255 / BELL_SLIDE_MICROS) as u16
}

/// How much of `cell` the bell colour covers, from `0` to `255`, as it moves in
/// over the label or back out again.
pub(super) fn bell_coverage(slide: BellSlide, cell: usize, width: usize) -> u16 {
    let (progress, arriving) = match slide {
        BellSlide::Covered => return 255,
        BellSlide::In(progress) => (progress, true),
        BellSlide::Out(progress) => (progress, false),
    };
    // Ease the edge at sub-cell resolution, so partly covered cells blend and
    // the colour glides in rather than snapping one whole cell at a time.
    let label = width.max(1) as u128 * BELL_SUBCELL as u128;
    let travelled = smoothstep_over(u128::from(progress) * label / 255, label) as i64;
    let start = cell as i64 * BELL_SUBCELL;
    let covered = if arriving {
        // The colour arrives from the left, so it covers everything behind it.
        travelled - start
    } else {
        // It leaves to the right, uncovering the label from the left.
        start + BELL_SUBCELL - (label as i64 - travelled)
    };
    (covered.clamp(0, BELL_SUBCELL) * 255 / BELL_SUBCELL) as u16
}

pub(super) fn bell_render_token(elapsed: u128, repeat: bool) -> u64 {
    if !repeat {
        return elapsed.min(BELL_SHIMMER_MICROS) as u64;
    }
    let cycle_micros = BELL_SHIMMER_MICROS + BELL_BREAK_MICROS;
    let cycle = elapsed / cycle_micros;
    let state = if elapsed % cycle_micros < BELL_SHIMMER_MICROS {
        elapsed % cycle_micros
    } else {
        BELL_SHIMMER_MICROS
    };
    (cycle * (BELL_SHIMMER_MICROS + 1) + state) as u64
}

/// Ease-in-out curve over `0..=scale`.
fn smoothstep_over(value: u128, scale: u128) -> u128 {
    value * value * (3 * scale - 2 * value) / (scale * scale)
}

fn smoothstep(value: u16) -> u16 {
    smoothstep_over(u128::from(value), 255) as u16
}

fn blend_channel(from: u8, to: u8, amount: u16) -> u8 {
    let from = i32::from(from);
    let difference = i32::from(to) - from;
    (from + difference * i32::from(amount) / 255) as u8
}

pub(super) fn blend_rgb(from: Rgb, to: Rgb, amount: u16) -> Rgb {
    (
        blend_channel(from.0, to.0, amount),
        blend_channel(from.1, to.1, amount),
        blend_channel(from.2, to.2, amount),
    )
}

/// Colours for one cell of a bell label, blended over the `resting` background
/// and foreground the label carries when no bell is pending.
pub(super) fn bell_cell_colors(
    visual: BellVisual,
    cell: usize,
    width: usize,
    resting: (Rgb, Rgb),
    theme: &Theme,
) -> (Rgb, Rgb) {
    let (background, foreground) = bell_visual_colors(visual, cell, width, theme);
    let coverage = bell_coverage(visual.slide, cell, width);
    (
        blend_rgb(resting.0, background, coverage),
        blend_rgb(resting.1, foreground, coverage),
    )
}

pub(super) fn bell_visual_colors(
    visual: BellVisual,
    cell: usize,
    width: usize,
    theme: &Theme,
) -> (Rgb, Rgb) {
    let Some(elapsed) = visual.shimmer else {
        return (theme.bell_base, theme.bell_text);
    };
    let span = BELL_HIGHLIGHT_SPAN * BELL_SUBCELL;
    let last_cell = width.saturating_sub(1) as i64 * BELL_SUBCELL;
    let elapsed = elapsed.min(BELL_SHIMMER_MICROS);
    // Ease the position at sub-cell resolution, so the highlight glides rather
    // than stepping. It starts one span left of the label and ends one span
    // past its right edge, sliding fully in and fully out again.
    let travel = (last_cell + 2 * span) as u128;
    let eased = smoothstep_over(elapsed * travel / BELL_SHIMMER_MICROS, travel);
    let centre = -span + eased as i64;
    let distance = (cell as i64 * BELL_SUBCELL - centre).abs();
    let brightness = if distance >= span {
        0
    } else {
        smoothstep(((span - distance) * 255 / span) as u16)
    };
    let background = blend_rgb(theme.bell_base, theme.bell_highlight, brightness);
    (background, theme.bell_text)
}

/// Paints `label` with the bell's colours blended over the `resting` ones it
/// carries when no bell is pending.
pub(super) fn render_bell_label(
    frame: &mut Frame,
    (row, col): (u16, u16),
    label: &str,
    bell: BellLabel,
    theme: &Theme,
) {
    let mut encoded = [0; 4];
    for (cell, character) in label.chars().enumerate() {
        let (background, foreground) =
            bell_cell_colors(bell.visual, cell, bell.animation_width, bell.resting, theme);
        let mut attributes = CellAttributes::colors(foreground, background);
        attributes.bold = bell.bold;
        frame.set_cell(
            row,
            col + cell as u16,
            character.encode_utf8(&mut encoded),
            attributes,
        );
    }
}

/// How one bell label is painted: what the bell is doing, how wide the sweep
/// across it runs, and the colours it rests on between passes.
#[derive(Clone, Copy)]
pub(super) struct BellLabel {
    pub(super) visual: BellVisual,
    pub(super) animation_width: usize,
    pub(super) resting: (Rgb, Rgb),
    pub(super) bold: bool,
}
