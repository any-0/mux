//! Where a window's panes sit, and the borders between them.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    config::Theme,
    frame::{CellAttributes, Frame},
};

/// Denominator for a split's ratio.
const RATIO_SCALE: u16 = 1_000;

/// A split down the middle, which is how every new one starts.
pub(super) const EVEN_SPLIT: u16 = RATIO_SCALE / 2;

#[derive(Clone, Deserialize, Serialize)]
pub(super) enum PaneLayout {
    Pane(usize),
    Split {
        axis: SplitAxis,
        /// The first child's share of the split, in [`RATIO_SCALE`]ths of the
        /// space left once the divider has taken its line.
        ratio: u16,
        first: Box<PaneLayout>,
        second: Box<PaneLayout>,
    },
}

impl PaneLayout {
    pub(super) fn split(&mut self, pane_id: usize, new_pane_id: usize, axis: SplitAxis) -> bool {
        match self {
            Self::Pane(current) if *current == pane_id => {
                *self = Self::Split {
                    axis,
                    ratio: EVEN_SPLIT,
                    first: Box::new(Self::Pane(pane_id)),
                    second: Box::new(Self::Pane(new_pane_id)),
                };
                true
            }
            Self::Pane(_) => false,
            Self::Split { first, second, .. } => {
                first.split(pane_id, new_pane_id, axis) || second.split(pane_id, new_pane_id, axis)
            }
        }
    }

    pub(super) fn without(self, pane_id: usize) -> Option<Self> {
        match self {
            Self::Pane(current) => (current != pane_id).then_some(Self::Pane(current)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.without(pane_id), second.without(pane_id)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(layout), None) | (None, Some(layout)) => Some(layout),
                (None, None) => None,
            },
        }
    }

    /// The smallest extent along `axis` that still leaves every pane in this
    /// subtree a cell of its own.
    fn minimum_extent(&self, axis: SplitAxis) -> u16 {
        match self {
            Self::Pane(_) => 1,
            Self::Split {
                axis: split_axis,
                first,
                second,
                ..
            } if *split_axis == axis => first
                .minimum_extent(axis)
                .saturating_add(1)
                .saturating_add(second.minimum_extent(axis)),
            Self::Split { first, second, .. } => {
                first.minimum_extent(axis).max(second.minimum_extent(axis))
            }
        }
    }

    pub(super) fn contains(&self, pane_id: usize) -> bool {
        match self {
            Self::Pane(current) => *current == pane_id,
            Self::Split { first, second, .. } => {
                first.contains(pane_id) || second.contains(pane_id)
            }
        }
    }

    /// Moves the divider nearest above `pane_id` by `cells`, positive meaning
    /// down or right. Reports whether a divider actually moved.
    ///
    /// The innermost split wins, which is what makes repeated presses feel
    /// local: they move the border the pane is actually sitting against.
    pub(super) fn resize(
        &mut self,
        area: Rect,
        pane_id: usize,
        axis: SplitAxis,
        cells: i32,
    ) -> bool {
        let Self::Split {
            axis: split_axis,
            ratio,
            first,
            second,
        } = self
        else {
            return false;
        };
        let (first_area, second_area) = split_areas(*split_axis, *ratio, area);
        if first.resize(first_area, pane_id, axis, cells)
            || second.resize(second_area, pane_id, axis, cells)
        {
            return true;
        }
        if *split_axis != axis || !(first.contains(pane_id) || second.contains(pane_id)) {
            return false;
        }
        let total = match axis {
            SplitAxis::Horizontal => area.rows,
            SplitAxis::Vertical => area.cols,
        };
        let (current, divider, _) = split_extent(total, *ratio);
        if divider == 0 {
            // Two rows or fewer: there is no divider to move.
            return false;
        }
        let available = i32::from(total) - 1;
        // Neither side may be squeezed past what the panes nested inside it
        // need: one cell each, plus a line for every divider between them.
        let lowest = i32::from(first.minimum_extent(axis));
        let highest = available - i32::from(second.minimum_extent(axis));
        if lowest > highest {
            return false;
        }
        let target = (i32::from(current) + cells).clamp(lowest, highest);
        let moved = target != i32::from(current);
        // Rounded up, so the ratio divides back into the cell count that was
        // asked for rather than one short of it.
        let scale = i32::from(RATIO_SCALE);
        *ratio = ((target * scale + available - 1) / available).clamp(1, scale - 1) as u16;
        moved
    }

    pub(super) fn pane_ids(&self, ids: &mut Vec<usize>) {
        match self {
            Self::Pane(pane_id) => ids.push(*pane_id),
            Self::Split { first, second, .. } => {
                first.pane_ids(ids);
                second.pane_ids(ids);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PaneDirection {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Rect {
    pub(super) row: u16,
    pub(super) col: u16,
    pub(super) rows: u16,
    pub(super) cols: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Divider {
    pub(super) row: u16,
    pub(super) col: u16,
    pub(super) rows: u16,
    pub(super) cols: u16,
    pub(super) axis: SplitAxis,
}

pub(super) fn window_regions(
    layout: &PaneLayout,
    active_pane: usize,
    zoomed: bool,
    area: Rect,
) -> (Vec<(usize, Rect)>, Vec<Divider>) {
    if zoomed {
        return (vec![(active_pane, area)], Vec::new());
    }
    pane_layout_regions(layout, area)
}

pub(super) fn pane_layout_regions(
    layout: &PaneLayout,
    area: Rect,
) -> (Vec<(usize, Rect)>, Vec<Divider>) {
    let mut panes = Vec::new();
    let mut dividers = Vec::new();
    collect_pane_layout(layout, area, &mut panes, &mut dividers);
    (panes, dividers)
}

pub(super) fn preview_grid_rects(count: usize, area: Rect) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let mut best = None;
    for columns in 1..=count {
        let rows = count.div_ceil(columns);
        if columns > area.cols as usize || rows > area.rows as usize {
            continue;
        }
        let cell_width = (area.cols as usize - (columns - 1)) / columns;
        let cell_height = (area.rows as usize - (rows - 1)) / rows;
        let score = cell_width.min(cell_height.saturating_mul(2));
        let area_score = cell_width.saturating_mul(cell_height);
        if best.is_none_or(|(_, _, best_score, best_area)| {
            (score, area_score) > (best_score, best_area)
        }) {
            best = Some((columns, rows, score, area_score));
        }
    }
    let (columns, rows, _, _) = best.unwrap_or((1, count, 0, 0));
    let column_spans = grid_spans(area.cols, columns);
    let row_spans = grid_spans(area.rows, rows);
    (0..count)
        .map(|index| {
            let (col_offset, cols) = column_spans[index % columns];
            let (row_offset, rows) = row_spans[index / columns];
            Rect {
                row: area.row + row_offset,
                col: area.col + col_offset,
                rows,
                cols,
            }
        })
        .collect()
}

fn grid_spans(total: u16, parts: usize) -> Vec<(u16, u16)> {
    let gaps = parts.saturating_sub(1) as u16;
    let available = total.saturating_sub(gaps);
    let base = available / parts as u16;
    let extra = available % parts as u16;
    let mut offset = 0;
    (0..parts)
        .map(|index| {
            let length = base + u16::from((index as u16) < extra);
            let span = (offset, length);
            offset += length + 1;
            span
        })
        .collect()
}

pub(super) fn preview_grid_separator_positions(area: Rect, rects: &[Rect]) -> (Vec<u16>, Vec<u16>) {
    let right = area.col + area.cols;
    let bottom = area.row + area.rows;
    let mut vertical = Vec::new();
    let mut horizontal = Vec::new();
    for rect in rects {
        let col = rect.col + rect.cols;
        if col < right
            && rects.iter().any(|candidate| candidate.col == col + 1)
            && !vertical.contains(&col)
        {
            vertical.push(col);
        }
        let row = rect.row + rect.rows;
        if row < bottom
            && rects.iter().any(|candidate| candidate.row == row + 1)
            && !horizontal.contains(&row)
        {
            horizontal.push(row);
        }
    }
    (vertical, horizontal)
}

/// Which sides of a divider cell carry a line, so a cell can pick the glyph
/// that joins them.
pub(super) const JOIN_UP: u8 = 1;

pub(super) const JOIN_DOWN: u8 = 2;

pub(super) const JOIN_LEFT: u8 = 4;

pub(super) const JOIN_RIGHT: u8 = 8;

/// Draws a window's dividers, joining the ones that meet.
///
/// Nested splits leave a divider ending against the side of another, so the
/// cell where they touch belongs to one divider but has to show both. Every
/// cell is collected before anything is painted, and each one then takes the
/// glyph for the sides that actually continue.
pub(super) fn render_dividers(
    frame: &mut Frame,
    dividers: &[Divider],
    bar_width: u16,
    attributes: CellAttributes,
) {
    for ((row, col), glyph) in divider_cells(dividers, bar_width) {
        frame.set_cell(row, col, glyph, attributes);
    }
}

/// The glyph every divider cell shows, in screen coordinates, once the
/// dividers that meet have been joined.
pub(super) fn divider_cells(
    dividers: &[Divider],
    bar_width: u16,
) -> Vec<((u16, u16), &'static str)> {
    let mut segments: HashMap<(u16, u16), u8> = HashMap::new();
    for divider in dividers {
        match divider.axis {
            SplitAxis::Horizontal => {
                for offset in 0..divider.cols {
                    let cell = (divider.row + 1, bar_width + divider.col + offset + 1);
                    *segments.entry(cell).or_default() |= JOIN_LEFT | JOIN_RIGHT;
                }
            }
            SplitAxis::Vertical => {
                for offset in 0..divider.rows {
                    let cell = (divider.row + offset + 1, bar_width + divider.col + 1);
                    *segments.entry(cell).or_default() |= JOIN_UP | JOIN_DOWN;
                }
            }
        }
    }
    let mut cells: Vec<_> = segments
        .iter()
        .map(|(&(row, col), &sides)| {
            let mut sides = sides;
            for (side, neighbour) in [
                (JOIN_UP, (row.wrapping_sub(1), col)),
                (JOIN_DOWN, (row + 1, col)),
                (JOIN_LEFT, (row, col.wrapping_sub(1))),
                (JOIN_RIGHT, (row, col + 1)),
            ] {
                if segments.contains_key(&neighbour) {
                    sides |= side;
                }
            }
            ((row, col), divider_glyph(sides))
        })
        .collect();
    cells.sort_unstable_by_key(|(cell, _)| *cell);
    cells
}

pub(super) fn divider_glyph(sides: u8) -> &'static str {
    match sides {
        sides if sides == JOIN_UP | JOIN_DOWN | JOIN_LEFT | JOIN_RIGHT => "┼",
        sides if sides == JOIN_UP | JOIN_DOWN | JOIN_LEFT => "┤",
        sides if sides == JOIN_UP | JOIN_DOWN | JOIN_RIGHT => "├",
        sides if sides == JOIN_LEFT | JOIN_RIGHT | JOIN_UP => "┴",
        sides if sides == JOIN_LEFT | JOIN_RIGHT | JOIN_DOWN => "┬",
        sides if sides == JOIN_UP | JOIN_RIGHT => "└",
        sides if sides == JOIN_UP | JOIN_LEFT => "┘",
        sides if sides == JOIN_DOWN | JOIN_RIGHT => "┌",
        sides if sides == JOIN_DOWN | JOIN_LEFT => "┐",
        sides if sides & (JOIN_UP | JOIN_DOWN) != 0 => "│",
        _ => "─",
    }
}

pub(super) fn render_preview_grid_separators(
    frame: &mut Frame,
    area: Rect,
    rects: &[Rect],
    theme: &Theme,
) {
    let (vertical, horizontal) = preview_grid_separator_positions(area, rects);
    let attributes = CellAttributes::foreground(theme.divider);
    for col in &vertical {
        for row in area.row..area.row + area.rows {
            let corner = horizontal.contains(&row);
            frame.set_cell(row, *col, if corner { "┼" } else { "│" }, attributes);
        }
    }
    for row in horizontal {
        for col in area.col..area.col + area.cols {
            let corner = vertical.contains(&col);
            frame.set_cell(row, col, if corner { "┼" } else { "─" }, attributes);
        }
    }
}

fn collect_pane_layout(
    layout: &PaneLayout,
    area: Rect,
    panes: &mut Vec<(usize, Rect)>,
    dividers: &mut Vec<Divider>,
) {
    match layout {
        PaneLayout::Pane(pane_id) => panes.push((*pane_id, area)),
        PaneLayout::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let (first_area, second_area) = split_areas(*axis, *ratio, area);
            collect_pane_layout(first, first_area, panes, dividers);
            let divider = match axis {
                SplitAxis::Horizontal => Divider {
                    row: area.row + first_area.rows,
                    col: area.col,
                    rows: 1,
                    cols: area.cols,
                    axis: *axis,
                },
                SplitAxis::Vertical => Divider {
                    row: area.row,
                    col: area.col + first_area.cols,
                    rows: area.rows,
                    cols: 1,
                    axis: *axis,
                },
            };
            let has_divider = match axis {
                SplitAxis::Horizontal => split_extent(area.rows, *ratio).1 > 0,
                SplitAxis::Vertical => split_extent(area.cols, *ratio).1 > 0,
            };
            if has_divider {
                dividers.push(divider);
            }
            collect_pane_layout(second, second_area, panes, dividers);
        }
    }
}

/// Splits `total` into the first pane, the divider, and the second pane.
fn split_extent(total: u16, ratio: u16) -> (u16, u16, u16) {
    match total {
        0 => (0, 0, 0),
        1 => (1, 0, 0),
        2 => (1, 0, 1),
        _ => {
            let available = u32::from(total) - 1;
            let ratio = u32::from(ratio.clamp(1, RATIO_SCALE - 1));
            let first = (available * ratio / u32::from(RATIO_SCALE)).clamp(1, available - 1) as u16;
            (first, 1, total - first - 1)
        }
    }
}

/// The areas a split hands to its two children, ignoring the divider between.
fn split_areas(axis: SplitAxis, ratio: u16, area: Rect) -> (Rect, Rect) {
    match axis {
        SplitAxis::Horizontal => {
            let (first_rows, divider, second_rows) = split_extent(area.rows, ratio);
            (
                Rect {
                    rows: first_rows,
                    ..area
                },
                Rect {
                    row: area.row + first_rows + divider,
                    rows: second_rows,
                    ..area
                },
            )
        }
        SplitAxis::Vertical => {
            let (first_cols, divider, second_cols) = split_extent(area.cols, ratio);
            (
                Rect {
                    cols: first_cols,
                    ..area
                },
                Rect {
                    col: area.col + first_cols + divider,
                    cols: second_cols,
                    ..area
                },
            )
        }
    }
}

pub(super) fn neighboring_pane(
    regions: &[(usize, Rect)],
    active_pane: usize,
    previous_pane: Option<usize>,
    direction: PaneDirection,
) -> Option<usize> {
    let active = regions
        .iter()
        .find_map(|(pane_id, rect)| (*pane_id == active_pane).then_some(*rect))?;
    regions
        .iter()
        .filter(|(pane_id, rect)| *pane_id != active_pane && rect.rows > 0 && rect.cols > 0)
        .filter_map(|(pane_id, rect)| {
            let (in_direction, distance, overlap, center_distance) = match direction {
                PaneDirection::Left => (
                    rect.col + rect.cols <= active.col,
                    active.col.saturating_sub(rect.col + rect.cols),
                    ranges_overlap(rect.row, rect.rows, active.row, active.rows),
                    center_distance(rect.row, rect.rows, active.row, active.rows),
                ),
                PaneDirection::Right => (
                    active.col + active.cols <= rect.col,
                    rect.col.saturating_sub(active.col + active.cols),
                    ranges_overlap(rect.row, rect.rows, active.row, active.rows),
                    center_distance(rect.row, rect.rows, active.row, active.rows),
                ),
                PaneDirection::Up => (
                    rect.row + rect.rows <= active.row,
                    active.row.saturating_sub(rect.row + rect.rows),
                    ranges_overlap(rect.col, rect.cols, active.col, active.cols),
                    center_distance(rect.col, rect.cols, active.col, active.cols),
                ),
                PaneDirection::Down => (
                    active.row + active.rows <= rect.row,
                    rect.row.saturating_sub(active.row + active.rows),
                    ranges_overlap(rect.col, rect.cols, active.col, active.cols),
                    center_distance(rect.col, rect.cols, active.col, active.cols),
                ),
            };
            in_direction.then_some((
                (
                    !overlap,
                    distance,
                    Some(*pane_id) != previous_pane,
                    center_distance,
                ),
                *pane_id,
            ))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, pane_id)| pane_id)
}

fn ranges_overlap(
    first_start: u16,
    first_length: u16,
    second_start: u16,
    second_length: u16,
) -> bool {
    first_start < second_start + second_length && second_start < first_start + first_length
}

fn center_distance(
    first_start: u16,
    first_length: u16,
    second_start: u16,
    second_length: u16,
) -> u16 {
    (first_start * 2 + first_length).abs_diff(second_start * 2 + second_length)
}
