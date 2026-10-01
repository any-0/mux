//! Resizing a pane must never lose, duplicate or split text, and the cursor
//! must stay at the same place in the text, however the size changes.

use mux_harness::{
    escape,
    generate::{Feature, Generator},
};
use rand::{Rng, SeedableRng, rngs::SmallRng};

/// Every logical line (rows joined across wraps), oldest first, with the
/// cursor's line and column offset within it.
fn logical_text(parser: &mut vt100::Parser) -> (Vec<String>, (usize, usize)) {
    let (rows, cols) = parser.screen().size();
    parser.screen_mut().set_scrollback(usize::MAX);
    let history = parser.screen().scrollback();
    let mut physical: Vec<(Vec<String>, bool)> = Vec::new();
    for index in 0..history {
        parser.screen_mut().set_scrollback(history - index);
        physical.push(row_cells(parser.screen(), 0, cols));
    }
    parser.screen_mut().set_scrollback(0);
    for row in 0..rows {
        physical.push(row_cells(parser.screen(), row, cols));
    }
    let (cursor_row, cursor_col) = parser.screen().cursor_position();
    let cursor_physical = history + usize::from(cursor_row);

    let mut lines = Vec::new();
    let mut line: Vec<String> = Vec::new();
    let mut cursor = (0, 0);
    for (index, (cells, wrapped)) in physical.into_iter().enumerate() {
        // The blank a wide character leaves when it wraps is not text.
        if cells.first().is_some_and(|cell| {
            cell.chars()
                .next()
                .is_some_and(|c| unicode_width::UnicodeWidthChar::width(c) == Some(2))
        }) && line.last().is_some_and(|cell| cell == "\0")
        {
            line.pop();
        }
        if index == cursor_physical {
            cursor = (lines.len(), line.len() + usize::from(cursor_col));
        }
        line.extend(cells);
        if !wrapped {
            lines.push(finish(&mut line));
        }
    }
    if !line.is_empty() {
        lines.push(finish(&mut line));
    }
    // Blank lines after both the text and the cursor are not content.
    while lines.len() > cursor.0 + 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    (lines, cursor)
}

fn finish(line: &mut Vec<String>) -> String {
    let text: String = line
        .drain(..)
        .map(|cell| if cell == "\0" { " ".to_string() } else { cell })
        .collect();
    text.trim_end().to_string()
}

/// Each cell's text, `"\0"` for a never-written one; wide continuations are
/// left out so that a wide character counts once, as two columns.
fn row_cells(screen: &vt100::Screen, row: u16, cols: u16) -> (Vec<String>, bool) {
    let mut cells = Vec::new();
    for col in 0..cols {
        let cell = screen.cell(row, col).unwrap();
        if cell.is_wide_continuation() {
            cells.push(String::new());
            continue;
        }
        cells.push(if cell.has_contents() {
            cell.contents().to_string()
        } else {
            "\0".to_string()
        });
    }
    (cells, screen.row_wrapped(row))
}

#[test]
fn resizing_keeps_every_line_and_the_cursor_in_place() {
    let features = [Feature::Text, Feature::Unicode, Feature::Sgr];
    let seeds: u64 = std::env::var("MUX_RESIZE_SEEDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3000);
    for seed in 0..seeds {
        let mut rng = SmallRng::seed_from_u64(seed);
        let (rows, cols) = (rng.random_range(2..16), rng.random_range(2..40));
        let mut parser = vt100::Parser::new(rows, cols, 100_000);
        let mut generator = Generator::new(seed, rows, cols, &features);
        let mut stream = Vec::new();
        for _ in 0..rng.random_range(0..40) {
            stream.extend(generator.stream(rng.random_range(1..5)));
            stream.extend_from_slice(b"\r\n");
        }
        // Sometimes end mid-line, with the cursor after the text.
        if rng.random_bool(0.5) {
            stream.extend(generator.stream(rng.random_range(1..4)));
        }
        parser.process(&stream);
        let before = logical_text(&mut parser);
        let mut sizes = Vec::new();
        for _ in 0..rng.random_range(1..5) {
            let size = (rng.random_range(1..18), rng.random_range(2..45));
            sizes.push(size);
            parser.screen_mut().set_size(size.0, size.1);
            let after = logical_text(&mut parser);
            assert_eq!(
                before.0,
                after.0,
                "seed {seed}: text changed after {rows}x{cols} -> {sizes:?}\nstream {}",
                escape(&stream)
            );
            assert_eq!(
                before.1,
                after.1,
                "seed {seed}: cursor moved in the text after {rows}x{cols} -> {sizes:?}\nstream {}",
                escape(&stream)
            );
        }
    }
}
