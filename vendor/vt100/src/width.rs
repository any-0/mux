use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_width::UnicodeWidthChar;

/// The number of terminal cells occupied by a Unicode scalar.
///
/// Terminal combining and format characters attach to the preceding cell.
/// `unicode-width` also models typographic widths (including three-column
/// characters and visible prepended format marks), which do not describe our
/// terminal's zero-, one-, and two-cell representation.
#[must_use]
pub fn character_width(c: char) -> Option<usize> {
    let width = c.width()?;
    if c.is_ascii() {
        return Some(width);
    }
    match get_general_category(c) {
        GeneralCategory::Format
        | GeneralCategory::NonspacingMark
        | GeneralCategory::SpacingMark
        | GeneralCategory::EnclosingMark => Some(0),
        _ => Some(if width > 2 { 1 } else { width }),
    }
}

/// Whether `c` is a format control unsafe for the enclosing terminal's stream.
///
/// Join controls and emoji tags
/// remain part of their base character's cell; other format controls can
/// change ordering or join the next write across an explicit cursor move.
#[must_use]
pub fn is_format_control(c: char) -> bool {
    !c.is_ascii()
        && !matches!(c, '\u{200c}' | '\u{200d}' | '\u{e0020}'..='\u{e007f}')
        && get_general_category(c) == GeneralCategory::Format
}
