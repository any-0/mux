use std::{
    cmp::{max, min},
    collections::HashMap,
    ops::Range,
};

use crate::{
    config::Action,
    protocol::{Key, KeyCode},
    server::snapshot::VimBuffer,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Position {
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelectionKind {
    Character,
    Line,
    Block,
}

#[derive(Clone, Debug)]
struct Selection {
    anchor: Position,
    kind: SelectionKind,
}

#[derive(Clone, Copy, Debug)]
struct Find {
    character: char,
    forward: bool,
    till: bool,
}

#[derive(Clone, Debug)]
struct Search {
    query: String,
    forward: bool,
}

/// A command waiting for its next key. `yank_from` is set when the command is
/// the motion of a `y` operator, and holds where the yank starts.
#[derive(Clone, Debug)]
enum Pending {
    None,
    JumpCharacter,
    JumpTarget {
        targets: Vec<JumpTarget>,
        typed: String,
    },
    GoTop {
        count: usize,
        yank_from: Option<Position>,
    },
    Find {
        forward: bool,
        till: bool,
        count: usize,
        yank_from: Option<Position>,
    },
    Search {
        forward: bool,
        query: String,
        count: usize,
    },
    Yank {
        count: usize,
    },
}

#[derive(Clone, Debug)]
struct JumpTarget {
    label: String,
    position: Position,
}

const JUMP_KEYS: &str = "asdghklqwertzuiopxycvbnmfj";
const JUMP_LIST_CAPACITY: usize = 100;

fn assign_jump_labels(positions: &[Position], prefix: &str, targets: &mut Vec<JumpTarget>) {
    if positions.is_empty() {
        return;
    }

    let keys: Vec<_> = JUMP_KEYS.chars().collect();
    if positions.len() <= keys.len() {
        targets.extend(
            positions
                .iter()
                .zip(keys)
                .map(|(position, key)| JumpTarget {
                    label: format!("{prefix}{key}"),
                    position: *position,
                }),
        );
        return;
    }

    // EasyMotion's SCTree grouping keeps the closest targets on single keys and
    // turns the last keys into prefixes as more label capacity is needed.
    let mut counts = vec![0; keys.len()];
    let mut remaining = positions.len();
    let mut level = 0;
    while remaining > 0 {
        let group_capacity = if level == 0 { 1 } else { keys.len() - 1 };
        for count in &mut counts {
            let take = remaining.min(group_capacity);
            *count += take;
            remaining -= take;
            if remaining == 0 {
                break;
            }
        }
        level += 1;
    }
    counts.reverse();

    let mut start = 0;
    for (key, count) in keys.into_iter().zip(counts).filter(|(_, count)| *count > 0) {
        let end = start + count;
        let label = format!("{prefix}{key}");
        if count == 1 {
            targets.push(JumpTarget {
                label,
                position: positions[start],
            });
        } else {
            assign_jump_labels(&positions[start..end], &label, targets);
        }
        start = end;
    }
}

/// `(forward, till)` for the `f`, `F`, `t` and `T` actions.
fn find_direction(action: Action) -> Option<(bool, bool)> {
    match action {
        Action::FindForward => Some((true, false)),
        Action::FindBackward => Some((false, false)),
        Action::TillForward => Some((true, true)),
        Action::TillBackward => Some((false, true)),
        _ => None,
    }
}

/// `(down, lines)` for the actions that move the cursor straight up or down.
fn vertical_step(action: Action, half_page: usize) -> Option<(bool, usize)> {
    match action {
        Action::CursorDown => Some((true, 1)),
        Action::CursorUp => Some((false, 1)),
        Action::CursorDown3 => Some((true, 3)),
        Action::CursorUp3 => Some((false, 3)),
        Action::CursorDown10 => Some((true, 10)),
        Action::CursorUp10 => Some((false, 10)),
        Action::HalfPageDown | Action::HalfPageDownCenter => Some((true, half_page)),
        Action::HalfPageUp | Action::HalfPageUpCenter => Some((false, half_page)),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub enum VimOutcome {
    None,
    Exit,
    Yank(String),
}

#[derive(Clone, Debug)]
pub struct VimMode {
    buffer: VimBuffer,
    pub cursor: Position,
    pub viewport_top: usize,
    viewport_height: usize,
    count: Option<usize>,
    pending: Pending,
    selection: Option<Selection>,
    last_find: Option<Find>,
    last_search: Option<Search>,
    message: Option<String>,
    /// Every match of the pattern being searched for, by row, as character
    /// column ranges. Recomputed as the pattern is typed so the highlight
    /// follows the prompt.
    search_matches: HashMap<usize, Vec<Range<usize>>>,
    jump_list: Vec<Position>,
    jump_index: usize,
}

#[derive(Clone, Copy)]
struct Motion {
    destination: Position,
    inclusive: bool,
    linewise: bool,
}

impl VimMode {
    pub fn buffer(&self) -> &VimBuffer {
        &self.buffer
    }

    pub fn new(buffer: VimBuffer, cursor: Position, viewport_height: usize) -> Self {
        let buffer = if buffer.len() == 0 {
            VimBuffer::blank()
        } else {
            buffer
        };
        let mut mode = Self {
            buffer,
            cursor,
            viewport_top: 0,
            viewport_height: viewport_height.max(1),
            count: None,
            pending: Pending::None,
            selection: None,
            last_find: None,
            last_search: None,
            message: None,
            search_matches: HashMap::new(),
            jump_list: Vec::new(),
            jump_index: 0,
        };
        mode.cursor = mode.clamp(mode.cursor);
        mode.viewport_top = mode.lowest_top();
        mode.ensure_visible();
        mode
    }

    pub fn prompt(&self) -> Option<String> {
        match &self.pending {
            Pending::Search { forward, query, .. } => {
                Some(format!("{}{}", if *forward { '/' } else { '?' }, query))
            }
            Pending::JumpCharacter => Some("jump to character".into()),
            Pending::JumpTarget { .. } => None,
            _ => self.message.clone(),
        }
    }

    pub fn jump_hint(&self, position: Position) -> Option<&str> {
        let Pending::JumpTarget { targets, typed } = &self.pending else {
            return None;
        };
        targets
            .iter()
            .find(|target| target.position == position && target.label.starts_with(typed))
            .map(|target| &target.label[typed.len()..])
    }

    /// Whether `position` falls inside a match of the pattern being searched
    /// for or last searched for.
    pub fn search_match(&self, position: Position) -> bool {
        self.search_matches
            .get(&position.row)
            .is_some_and(|ranges| ranges.iter().any(|range| range.contains(&position.col)))
    }

    /// Whether `position` falls inside the match the cursor is sitting on.
    pub fn current_search_match(&self, position: Position) -> bool {
        position.row == self.cursor.row
            && self
                .search_matches
                .get(&position.row)
                .is_some_and(|ranges| {
                    ranges.iter().any(|range| {
                        range.start == self.cursor.col && range.contains(&position.col)
                    })
                })
    }

    /// Indexes every match of `query` for the renderer, returning where each
    /// one starts in buffer order.
    fn highlight_search(&mut self, query: &str) -> Vec<Position> {
        self.search_matches.clear();
        if query.is_empty() {
            return Vec::new();
        }
        let matches: Vec<_> = self
            .buffer
            .texts()
            .enumerate()
            .flat_map(|(row, line)| {
                line.match_indices(query).map(move |(byte, _)| Position {
                    row,
                    col: line[..byte].chars().count(),
                })
            })
            .collect();
        let length = query.chars().count();
        for start in &matches {
            self.search_matches
                .entry(start.row)
                .or_default()
                .push(start.col..start.col + length);
        }
        matches
    }

    /// The selection's top-left and bottom-right corners, as a block.
    fn selection_corners(&self, selection: &Selection) -> (Position, Position) {
        let (a, b) = (selection.anchor, self.cursor);
        (
            Position {
                row: min(a.row, b.row),
                col: min(a.col, b.col),
            },
            Position {
                row: max(a.row, b.row),
                col: max(a.col, b.col),
            },
        )
    }

    pub fn selected(&self, position: Position) -> bool {
        let Some(selection) = &self.selection else {
            return false;
        };
        let (top, bottom) = self.selection_corners(selection);
        let rows = (top.row..=bottom.row).contains(&position.row);
        match selection.kind {
            SelectionKind::Character => (min(selection.anchor, self.cursor)
                ..=max(selection.anchor, self.cursor))
                .contains(&position),
            SelectionKind::Line => rows,
            SelectionKind::Block => rows && (top.col..=bottom.col).contains(&position.col),
        }
    }

    /// Asks for the character to jump to, as the jump key does. Entering the
    /// mode on a jump starts here rather than replaying a key press.
    pub fn start_jump(&mut self) {
        self.count = None;
        self.pending = Pending::JumpCharacter;
    }

    pub fn handle(&mut self, action: Option<Action>, key: &Key) -> VimOutcome {
        self.message = None;

        match self.pending {
            Pending::Search { .. } => return self.handle_search_prompt(key),
            Pending::JumpCharacter | Pending::JumpTarget { .. } => return self.handle_jump(key),
            Pending::Find { .. } => return self.handle_find_character(key),
            _ => {}
        }

        if let Key {
            code: KeyCode::Char(digit @ '0'..='9'),
            modifiers: 0,
        } = key
            && (*digit != '0' || self.count.is_some())
        {
            let digit = digit.to_digit(10).unwrap() as usize;
            self.count = Some(
                self.count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit),
            );
            return VimOutcome::None;
        }

        let Some(action) = action else {
            self.pending = Pending::None;
            self.count = None;
            return VimOutcome::None;
        };

        if let Pending::Yank {
            count: operator_count,
        } = self.pending
        {
            let count = operator_count.saturating_mul(self.take_count());
            if action == Action::Yank {
                self.pending = Pending::None;
                return VimOutcome::Yank(self.yank_lines(self.cursor.row, count));
            }
            let start = Some(self.cursor);
            if let Some((forward, till)) = find_direction(action) {
                self.pending = Pending::Find {
                    forward,
                    till,
                    count,
                    yank_from: start,
                };
                return VimOutcome::None;
            }
            if action == Action::GoTop {
                self.pending = Pending::GoTop {
                    count,
                    yank_from: start,
                };
                return VimOutcome::None;
            }
            self.pending = Pending::None;
            return match self.motion(action, count) {
                Some(motion) => VimOutcome::Yank(self.yank_motion(self.cursor, motion)),
                None => VimOutcome::None,
            };
        }

        if let Pending::GoTop { count, yank_from } = self.pending {
            self.pending = Pending::None;
            let motion = self.line_motion(count, 0);
            match yank_from {
                Some(start) if action == Action::GoTop => {
                    return VimOutcome::Yank(self.yank_motion(start, motion));
                }
                // A yank waiting for `gg` swallows any other key.
                Some(_) => return VimOutcome::None,
                None if action == Action::GoTop => {
                    let start = self.cursor;
                    self.move_cursor(motion);
                    self.record_jump(start);
                    return VimOutcome::None;
                }
                None => {}
            }
        }

        if let Some((forward, till)) = find_direction(action) {
            let count = self.take_count();
            self.pending = Pending::Find {
                forward,
                till,
                count,
                yank_from: None,
            };
            return VimOutcome::None;
        }

        match action {
            Action::Escape => {
                self.count = None;
                self.pending = Pending::None;
                let highlighted = !self.search_matches.is_empty();
                self.search_matches.clear();
                if self.selection.take().is_some() || highlighted {
                    VimOutcome::None
                } else {
                    VimOutcome::Exit
                }
            }
            Action::Visual | Action::VisualLine | Action::VisualBlock => {
                self.toggle_selection(match action {
                    Action::Visual => SelectionKind::Character,
                    Action::VisualLine => SelectionKind::Line,
                    _ => SelectionKind::Block,
                });
                VimOutcome::None
            }
            Action::Yank => {
                if self.selection.is_some() {
                    VimOutcome::Yank(self.yank_selection())
                } else {
                    let count = self.take_count();
                    self.pending = Pending::Yank { count };
                    VimOutcome::None
                }
            }
            Action::YankToLineEnd => {
                let count = self.take_count();
                let row = self
                    .cursor
                    .row
                    .saturating_add(count - 1)
                    .min(self.last_row());
                VimOutcome::Yank(self.yank_motion(
                    self.cursor,
                    Motion {
                        destination: Position {
                            row,
                            col: self.line_end(row),
                        },
                        inclusive: true,
                        linewise: false,
                    },
                ))
            }
            Action::GoTop => {
                let count = self.take_count();
                self.pending = Pending::GoTop {
                    count,
                    yank_from: None,
                };
                VimOutcome::None
            }
            Action::SearchForward | Action::SearchBackward => {
                let count = self.take_count();
                self.pending = Pending::Search {
                    forward: action == Action::SearchForward,
                    query: String::new(),
                    count,
                };
                VimOutcome::None
            }
            Action::RepeatFindForward | Action::RepeatFindBackward => {
                let count = self.take_count();
                if let Some(mut find) = self.last_find {
                    if action == Action::RepeatFindBackward {
                        find.forward = !find.forward;
                    }
                    self.apply_find(find, count);
                }
                VimOutcome::None
            }
            Action::RepeatSearch | Action::RepeatSearchReverse => {
                let count = self.take_count();
                if let Some(search) = self.last_search.clone() {
                    let start = self.cursor;
                    let forward = search.forward == (action == Action::RepeatSearch);
                    self.apply_search(&search.query, forward, count);
                    self.record_jump(start);
                }
                VimOutcome::None
            }
            Action::JumpCharacter => {
                self.start_jump();
                VimOutcome::None
            }
            Action::JumpOlder | Action::JumpNewer => {
                let count = self.take_count();
                self.navigate_jumps(action == Action::JumpOlder, count);
                VimOutcome::None
            }
            Action::HalfPageDownCenter | Action::HalfPageUpCenter => {
                let count = self.take_count();
                let start = self.cursor;
                if let Some(motion) = self.motion(action, count) {
                    self.cursor = self.clamp(motion.destination);
                    self.viewport_top = self
                        .cursor
                        .row
                        .saturating_sub(self.viewport_height / 2)
                        .min(self.lowest_top());
                    self.record_jump(start);
                }
                VimOutcome::None
            }
            Action::HalfPageDown | Action::HalfPageUp => {
                let count = self.take_count();
                let start = self.cursor;
                let distance = (self.viewport_height / 2).max(1).saturating_mul(count);
                if let Some(motion) = self.motion(action, count) {
                    self.cursor = self.clamp(motion.destination);
                    self.viewport_top = if action == Action::HalfPageDown {
                        self.viewport_top.saturating_add(distance)
                    } else {
                        self.viewport_top.saturating_sub(distance)
                    }
                    .min(self.lowest_top());
                    self.ensure_visible();
                    self.record_jump(start);
                }
                VimOutcome::None
            }
            _ => {
                let count = self.take_count();
                if let Some(motion) = self.motion(action, count) {
                    let start = self.cursor;
                    self.move_cursor(motion);
                    if action == Action::GoBottom {
                        self.record_jump(start);
                    }
                }
                VimOutcome::None
            }
        }
    }

    fn handle_jump(&mut self, key: &Key) -> VimOutcome {
        let pending = std::mem::replace(&mut self.pending, Pending::None);
        let Key {
            code: KeyCode::Char(character),
            modifiers: 0,
        } = key
        else {
            return VimOutcome::None;
        };

        match pending {
            Pending::JumpCharacter => {
                let targets = self.jump_targets(*character);
                if targets.is_empty() {
                    self.message = Some(format!("character {character:?} not visible"));
                } else {
                    self.pending = Pending::JumpTarget {
                        targets,
                        typed: String::new(),
                    };
                }
            }
            Pending::JumpTarget { targets, mut typed } => {
                typed.push(*character);
                if let Some(target) = targets.iter().find(|target| target.label == typed) {
                    let start = self.cursor;
                    self.cursor = target.position;
                    self.ensure_visible();
                    self.record_jump(start);
                } else if targets
                    .iter()
                    .any(|target| target.label.starts_with(&typed))
                {
                    self.pending = Pending::JumpTarget { targets, typed };
                }
            }
            _ => unreachable!(),
        }
        VimOutcome::None
    }

    /// Every visible occurrence of `character`, labelled nearest first,
    /// alternating forward and backward from the cursor.
    fn jump_targets(&self, character: char) -> Vec<JumpTarget> {
        let visible_end = min(
            self.viewport_top.saturating_add(self.viewport_height),
            self.buffer.len(),
        );
        let mut forward = Vec::new();
        let mut backward = Vec::new();
        for row in self.viewport_top..visible_end {
            for (col, candidate) in self.buffer.text(row).chars().enumerate() {
                let position = Position { row, col };
                if candidate != character || position == self.cursor {
                    continue;
                }
                if position > self.cursor {
                    forward.push(position);
                } else {
                    backward.push(position);
                }
            }
        }
        backward.reverse();

        let mut positions = Vec::with_capacity(forward.len() + backward.len());
        for index in 0..max(forward.len(), backward.len()) {
            positions.extend(forward.get(index));
            positions.extend(backward.get(index));
        }
        let mut targets = Vec::with_capacity(positions.len());
        assign_jump_labels(&positions, "", &mut targets);
        targets
    }

    fn handle_search_prompt(&mut self, key: &Key) -> VimOutcome {
        let Pending::Search { query, .. } = &mut self.pending else {
            return VimOutcome::None;
        };
        match &key.code {
            KeyCode::Escape => {
                self.pending = Pending::None;
                self.search_matches.clear();
            }
            KeyCode::Backspace => {
                query.pop();
                let query = query.clone();
                self.highlight_search(&query);
            }
            KeyCode::Char(character) if key.modifiers == 0 => {
                query.push(*character);
                let query = query.clone();
                self.highlight_search(&query);
            }
            KeyCode::Enter => {
                let pending = std::mem::replace(&mut self.pending, Pending::None);
                if let Pending::Search {
                    forward,
                    query,
                    count,
                } = pending
                    && !query.is_empty()
                {
                    let start = self.cursor;
                    self.apply_search(&query, forward, count);
                    self.last_search = Some(Search { query, forward });
                    self.record_jump(start);
                }
            }
            _ => {}
        }
        VimOutcome::None
    }

    fn handle_find_character(&mut self, key: &Key) -> VimOutcome {
        let pending = std::mem::replace(&mut self.pending, Pending::None);
        let (
            Key {
                code: KeyCode::Char(character),
                modifiers: 0,
            },
            Pending::Find {
                forward,
                till,
                count,
                yank_from,
            },
        ) = (key, pending)
        else {
            return VimOutcome::None;
        };
        let find = Find {
            character: *character,
            forward,
            till,
        };
        self.last_find = Some(find);
        let found = self.apply_find(find, count);
        match yank_from {
            Some(start) if found => VimOutcome::Yank(self.yank_motion(
                start,
                Motion {
                    destination: self.cursor,
                    inclusive: true,
                    linewise: false,
                },
            )),
            _ => VimOutcome::None,
        }
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1)
    }

    fn toggle_selection(&mut self, kind: SelectionKind) {
        match &mut self.selection {
            Some(selection) if selection.kind == kind => self.selection = None,
            Some(selection) => selection.kind = kind,
            None => {
                self.selection = Some(Selection {
                    anchor: self.cursor,
                    kind,
                })
            }
        }
    }

    /// Scrolls the viewport by `lines`, keeping the cursor on screen.
    ///
    /// Reports whether it moved: at the bottom of the buffer there is nothing
    /// left to scroll to, which is the caller's cue to leave vim mode.
    pub fn scroll(&mut self, up: bool, lines: usize) -> bool {
        let target = if up {
            self.viewport_top.saturating_sub(lines)
        } else {
            self.viewport_top
                .saturating_add(lines)
                .min(self.lowest_top())
        };
        if target == self.viewport_top {
            return false;
        }
        self.viewport_top = target;
        // The cursor follows the view rather than the other way round.
        self.cursor.row = self
            .cursor
            .row
            .clamp(target, target + self.viewport_height.saturating_sub(1))
            .min(self.last_row());
        self.cursor = self.clamp(self.cursor);
        true
    }

    fn move_cursor(&mut self, motion: Motion) {
        self.cursor = self.clamp(motion.destination);
        self.ensure_visible();
    }

    fn record_jump(&mut self, start: Position) {
        if start == self.cursor {
            return;
        }
        self.jump_list.truncate(self.jump_index.saturating_add(1));
        if self.jump_list.get(self.jump_index) != Some(&start) {
            self.jump_list.push(start);
        }
        if self.jump_list.last() != Some(&self.cursor) {
            self.jump_list.push(self.cursor);
        }
        let excess = self.jump_list.len().saturating_sub(JUMP_LIST_CAPACITY);
        self.jump_list.drain(..excess);
        self.jump_index = self.jump_list.len().saturating_sub(1);
    }

    fn navigate_jumps(&mut self, older: bool, count: usize) {
        if self.jump_list.is_empty() {
            return;
        }
        self.jump_index = if older {
            self.jump_index.saturating_sub(count)
        } else {
            self.jump_index
                .saturating_add(count)
                .min(self.jump_list.len() - 1)
        };
        self.cursor = self.clamp(self.jump_list[self.jump_index]);
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.cursor.row < self.viewport_top {
            self.viewport_top = self.cursor.row;
        }
        if self.cursor.row >= self.viewport_top + self.viewport_height {
            self.viewport_top = self.cursor.row + 1 - self.viewport_height;
        }
        self.viewport_top = self.viewport_top.min(self.lowest_top());
    }

    /// The viewport top that shows the end of the buffer.
    fn lowest_top(&self) -> usize {
        self.buffer.len().saturating_sub(self.viewport_height)
    }

    fn last_row(&self) -> usize {
        self.buffer.len() - 1
    }

    fn clamp(&self, mut position: Position) -> Position {
        position.row = position.row.min(self.last_row());
        position.col = position.col.min(self.line_end(position.row));
        position
    }

    fn line_len(&self, row: usize) -> usize {
        self.buffer.text(row).chars().count()
    }

    fn line_end(&self, row: usize) -> usize {
        self.line_len(row).saturating_sub(1)
    }

    fn first_nonblank(&self, row: usize) -> usize {
        self.buffer
            .text(row)
            .chars()
            .position(|character| !character.is_whitespace())
            .unwrap_or(0)
    }

    /// `gg` and `G`: line `count`, or `default_row` when no count was given.
    fn line_motion(&self, count: usize, default_row: usize) -> Motion {
        let row = if count == 1 {
            default_row
        } else {
            (count - 1).min(self.last_row())
        };
        Motion {
            destination: Position {
                row,
                col: self.first_nonblank(row),
            },
            inclusive: false,
            linewise: true,
        }
    }

    fn motion(&self, action: Action, count: usize) -> Option<Motion> {
        let mut position = self.cursor;
        let half_page = (self.viewport_height / 2).max(1);
        if let Some((down, lines)) = vertical_step(action, half_page) {
            let distance = lines.saturating_mul(count);
            position.row = if down {
                position.row.saturating_add(distance).min(self.last_row())
            } else {
                position.row.saturating_sub(distance)
            };
            return Some(Motion {
                destination: self.clamp(position),
                inclusive: false,
                linewise: true,
            });
        }
        let big = matches!(
            action,
            Action::BigWordForward | Action::BigWordEnd | Action::BigWordBackward
        );
        let inclusive = match action {
            Action::CursorLeft => {
                position.col = position.col.saturating_sub(count);
                true
            }
            Action::CursorRight => {
                position.col = min(
                    position.col.saturating_add(count),
                    self.line_end(position.row),
                );
                true
            }
            Action::WordForward | Action::BigWordForward => {
                position = self.word_motion(position, |text, index| {
                    word_forward(text, index, count, big)
                });
                false
            }
            Action::WordEnd | Action::BigWordEnd => {
                position =
                    self.word_motion(position, |text, index| word_end(text, index, count, big));
                true
            }
            Action::WordBackward | Action::BigWordBackward => {
                position = self.word_motion(position, |text, index| {
                    word_backward(text, index, count, big)
                });
                false
            }
            Action::LineStart => {
                position.col = 0;
                false
            }
            Action::FirstNonBlank => {
                position.col = self.first_nonblank(position.row);
                false
            }
            Action::LineEnd => {
                position.col = self.line_end(position.row);
                true
            }
            Action::GoBottom => return Some(self.line_motion(count, self.last_row())),
            _ => return None,
        };
        Some(Motion {
            destination: self.clamp(position),
            inclusive,
            linewise: false,
        })
    }

    /// The whole buffer as characters, lines joined by `\n`.
    fn flat(&self) -> Vec<char> {
        let mut result = Vec::new();
        for (index, line) in self.buffer.texts().enumerate() {
            if index > 0 {
                result.push('\n');
            }
            result.extend(line.chars());
        }
        result
    }

    fn position_index(&self, position: Position) -> usize {
        self.buffer
            .texts()
            .take(position.row)
            .map(|line| line.chars().count() + 1)
            .sum::<usize>()
            + position.col.min(self.line_len(position.row))
    }

    fn index_position(&self, index: usize) -> Position {
        let mut remaining = index;
        for (row, line) in self.buffer.texts().enumerate() {
            let len = line.chars().count();
            if remaining < len {
                return Position {
                    row,
                    col: remaining,
                };
            }
            if remaining == len {
                // The newline: it belongs to the start of the next line.
                return if row < self.last_row() {
                    Position {
                        row: row + 1,
                        col: 0,
                    }
                } else {
                    Position {
                        row,
                        col: len.saturating_sub(1),
                    }
                };
            }
            remaining -= len + 1;
        }
        let row = self.last_row();
        Position {
            row,
            col: self.line_end(row),
        }
    }

    /// Runs a word motion over the flattened buffer.
    fn word_motion(&self, start: Position, step: impl FnOnce(&[char], usize) -> usize) -> Position {
        let text = self.flat();
        if text.is_empty() {
            return start;
        }
        let index = self.position_index(start).min(text.len() - 1);
        self.index_position(step(&text, index))
    }

    fn apply_find(&mut self, find: Find, count: usize) -> bool {
        let characters: Vec<_> = self.buffer.text(self.cursor.row).chars().collect();
        let is_target = |index: &usize| characters[*index] == find.character;
        let found = if find.forward {
            (self.cursor.col + 1..characters.len())
                .filter(is_target)
                .nth(count - 1)
        } else {
            (0..self.cursor.col).rev().filter(is_target).nth(count - 1)
        };
        let Some(mut col) = found else {
            self.message = Some(format!("character {:?} not found", find.character));
            return false;
        };
        if find.till {
            col = if find.forward {
                col.saturating_sub(1)
            } else {
                min(col + 1, self.line_end(self.cursor.row))
            };
        }
        self.cursor.col = col;
        self.ensure_visible();
        true
    }

    fn apply_search(&mut self, query: &str, forward: bool, count: usize) {
        let matches = self.highlight_search(query);
        let (Some(&first), Some(&last)) = (matches.first(), matches.last()) else {
            self.message = Some(format!("pattern not found: {query}"));
            return;
        };
        let mut current = self.cursor;
        for _ in 0..count {
            current = if forward {
                matches
                    .iter()
                    .copied()
                    .find(|position| *position > current)
                    .unwrap_or(first)
            } else {
                matches
                    .iter()
                    .rev()
                    .copied()
                    .find(|position| *position < current)
                    .unwrap_or(last)
            };
        }
        self.cursor = current;
        self.ensure_visible();
    }

    fn yank_selection(&self) -> String {
        let selection = self.selection.as_ref().unwrap();
        let (top, bottom) = self.selection_corners(selection);
        match selection.kind {
            SelectionKind::Character => self.text_between(selection.anchor, self.cursor, true),
            SelectionKind::Line => self.yank_rows(top.row, bottom.row),
            SelectionKind::Block => (top.row..=bottom.row)
                .map(|row| {
                    self.buffer
                        .text(row)
                        .chars()
                        .skip(top.col)
                        .take(bottom.col - top.col + 1)
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    fn yank_lines(&self, start: usize, count: usize) -> String {
        let end = min(start.saturating_add(count), self.buffer.len());
        let mut text = (start..end)
            .map(|row| self.buffer.text(row))
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        text
    }

    /// Every line from `a` to `b`, in whichever order they come.
    fn yank_rows(&self, a: usize, b: usize) -> String {
        let first = min(a, b);
        self.yank_lines(first, max(a, b) - first + 1)
    }

    fn yank_motion(&self, start: Position, motion: Motion) -> String {
        if motion.linewise {
            self.yank_rows(start.row, motion.destination.row)
        } else {
            self.text_between(start, motion.destination, motion.inclusive)
        }
    }

    fn text_between(&self, a: Position, b: Position, inclusive: bool) -> String {
        let flat = self.flat();
        let a = self.position_index(a);
        let b = self.position_index(b);
        let (start, mut end) = (min(a, b), max(a, b));
        if inclusive {
            end = end.saturating_add(1);
        }
        flat[start.min(flat.len())..end.min(flat.len())]
            .iter()
            .collect()
    }
}

/// Whitespace, word characters, and punctuation, with `big` folding the last
/// two together as `W`, `E` and `B` do.
fn category(character: char, big: bool) -> u8 {
    if character.is_whitespace() {
        0
    } else if big || character.is_alphanumeric() || character == '_' {
        1
    } else {
        2
    }
}

fn word_forward(text: &[char], mut index: usize, count: usize, big: bool) -> usize {
    let last = text.len() - 1;
    let class = |index: usize| category(text[index], big);
    for _ in 0..count {
        let category = class(index);
        if category != 0 {
            while index < last && class(index + 1) == category {
                index += 1;
            }
            if index < last {
                index += 1;
            }
        }
        while index < last && class(index) == 0 {
            index += 1;
        }
    }
    index
}

fn word_end(text: &[char], mut index: usize, count: usize, big: bool) -> usize {
    let last = text.len() - 1;
    let class = |index: usize| category(text[index], big);
    for iteration in 0..count {
        if iteration > 0 && index < last {
            index += 1;
        }
        while index < last && class(index) == 0 {
            index += 1;
        }
        // Already on the end of a word, the first press moves on to the next.
        if iteration == 0 && index < last && class(index) != 0 && class(index + 1) != class(index) {
            index += 1;
            while index < last && class(index) == 0 {
                index += 1;
            }
        }
        let category = class(index);
        while index < last && category != 0 && class(index + 1) == category {
            index += 1;
        }
    }
    index
}

fn word_backward(text: &[char], mut index: usize, count: usize, big: bool) -> usize {
    let class = |index: usize| category(text[index], big);
    for _ in 0..count {
        index = index.saturating_sub(1);
        while index > 0 && class(index) == 0 {
            index -= 1;
        }
        let category = class(index);
        while index > 0 && class(index - 1) == category {
            index -= 1;
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Bindings, config::Mode, protocol::parse_for_test};

    fn mode(lines: Vec<String>, cursor: Position, viewport_height: usize) -> VimMode {
        VimMode::new(VimBuffer::from_text(lines), cursor, viewport_height)
    }

    fn numbered(count: usize, prefix: &str) -> Vec<String> {
        (0..count)
            .map(|number| format!("{prefix}{number}"))
            .collect()
    }

    fn at(row: usize, col: usize) -> Position {
        Position { row, col }
    }

    fn sample_vim() -> VimMode {
        mode(
            vec![
                "one two-three".into(),
                "  alpha beta alpha".into(),
                "last line".into(),
            ],
            at(0, 0),
            2,
        )
    }

    /// Presses each space-separated key name in turn, returning the last
    /// outcome. An empty name is the space bar, so `" a"` is space then `a`.
    fn press(vim: &mut VimMode, keys: &str) -> VimOutcome {
        let bindings = Bindings::defaults();
        let mut outcome = VimOutcome::None;
        for name in keys.split(' ') {
            let key = parse_for_test(if name.is_empty() { " " } else { name });
            outcome = vim.handle(bindings.get(Mode::Vim, &key), &key);
        }
        outcome
    }

    fn type_text(vim: &mut VimMode, text: &str) {
        for character in text.chars() {
            press(vim, &character.to_string());
        }
    }

    impl VimOutcome {
        fn yank(self) -> String {
            match self {
                Self::Yank(text) => text,
                other => panic!("expected yank, got {other:?}"),
            }
        }
    }

    #[test]
    fn yy_completes_the_operator() {
        let mut vim = sample_vim();
        assert_eq!(press(&mut vim, "y y").yank(), "one two-three\n");
        // The next motion just moves; it does not yank again.
        assert!(matches!(press(&mut vim, "j"), VimOutcome::None));
        assert_eq!(vim.cursor, at(1, 0));
    }

    #[test]
    fn counts_and_word_motions() {
        let mut vim = sample_vim();
        press(&mut vim, "2 w");
        assert_eq!(vim.cursor, at(0, 7));
        press(&mut vim, "b");
        assert_eq!(vim.cursor, at(0, 4));
        press(&mut vim, "e");
        assert_eq!(vim.cursor, at(0, 6));
    }

    #[test]
    fn word_and_big_word_classes_differ() {
        let mut vim = mode(vec!["one-two three".into()], at(0, 0), 1);
        press(&mut vim, "w");
        assert_eq!(vim.cursor.col, 3);
        press(&mut vim, "0 W");
        assert_eq!(vim.cursor.col, 8);
        press(&mut vim, "B");
        assert_eq!(vim.cursor.col, 0);
        press(&mut vim, "E");
        assert_eq!(vim.cursor.col, 6);
    }

    #[test]
    fn word_motions_are_safe_on_an_empty_screen() {
        let mut vim = mode(vec![String::new()], at(0, 0), 1);
        press(&mut vim, "w e b");
        assert_eq!(vim.cursor, at(0, 0));
    }

    #[test]
    fn jumps_move_backward_and_forward_and_a_new_jump_discards_newer_ones() {
        let mut vim = mode(numbered(30, "line "), at(20, 0), 6);
        press(&mut vim, "Ctrl-u");
        assert_eq!(vim.cursor.row, 17);
        press(&mut vim, "G");
        assert_eq!(vim.cursor.row, 29);

        press(&mut vim, "2 Ctrl-o");
        assert_eq!(vim.cursor.row, 20);
        press(&mut vim, "Ctrl-l");
        assert_eq!(vim.cursor.row, 17);
        press(&mut vim, "Tab");
        assert_eq!(vim.cursor.row, 29);

        press(&mut vim, "Ctrl-o");
        assert_eq!(vim.cursor.row, 17);
        press(&mut vim, "g g");
        assert_eq!(vim.cursor.row, 0);
        press(&mut vim, "Ctrl-l");
        assert_eq!(vim.cursor.row, 0);
        press(&mut vim, "Ctrl-o");
        assert_eq!(vim.cursor.row, 17);
    }

    #[test]
    fn ordinary_motions_do_not_enter_the_jump_list() {
        let mut vim = sample_vim();
        press(&mut vim, "j w");
        let position = vim.cursor;
        press(&mut vim, "Ctrl-o");
        assert_eq!(vim.cursor, position);
    }

    #[test]
    fn the_wheel_scrolls_the_viewport_and_stops_at_the_bottom() {
        let mut vim = mode(numbered(20, "line "), at(19, 0), 6);
        // Entering vim mode starts at the end of the buffer.
        assert_eq!(vim.viewport_top, 14);
        assert!(vim.scroll(true, 3));
        assert_eq!(vim.viewport_top, 11);
        assert!(
            (11..17).contains(&vim.cursor.row),
            "the cursor came along with the view"
        );
        assert!(vim.scroll(false, 3));
        assert_eq!(vim.viewport_top, 14);
        // Already at the bottom: nothing to scroll to, which ends vim mode.
        assert!(!vim.scroll(false, 3));
        while vim.scroll(true, 5) {}
        assert_eq!(vim.viewport_top, 0);
    }

    #[test]
    fn line_vertical_page_and_file_motions() {
        let mut vim = mode(numbered(20, "  line "), at(10, 4), 6);
        press(&mut vim, "0");
        assert_eq!(vim.cursor.col, 0);
        press(&mut vim, "^");
        assert_eq!(vim.cursor.col, 2);
        press(&mut vim, "$");
        assert_eq!(vim.cursor.col, 8);
        press(&mut vim, "2 k");
        assert_eq!(vim.cursor.row, 8);
        press(&mut vim, "Ctrl-u");
        assert_eq!(vim.cursor.row, 5);
        press(&mut vim, "G");
        assert_eq!(vim.cursor, at(19, 2));
        press(&mut vim, "g g");
        assert_eq!(vim.cursor, at(0, 2));
        press(&mut vim, "5 G");
        assert_eq!(vim.cursor, at(4, 2));
    }

    #[test]
    fn arrow_keys_follow_basic_motions() {
        let mut vim = sample_vim();
        press(&mut vim, "Right");
        assert_eq!(vim.cursor, at(0, 1));
        press(&mut vim, "Down");
        assert_eq!(vim.cursor, at(1, 1));
        press(&mut vim, "Left");
        assert_eq!(vim.cursor, at(1, 0));
        press(&mut vim, "Up");
        assert_eq!(vim.cursor, at(0, 0));
    }

    #[test]
    fn find_till_and_swappable_repeats() {
        let mut vim = sample_vim();
        press(&mut vim, "f -");
        assert_eq!(vim.cursor.col, 7);
        press(&mut vim, ",");
        assert_eq!(vim.cursor.col, 7);
        press(&mut vim, "t e");
        assert_eq!(vim.cursor.col, 10);
    }

    #[test]
    fn space_labels_visible_character_matches_and_jumps_by_hint() {
        let mut vim = sample_vim();
        press(&mut vim, "");
        assert_eq!(vim.prompt().as_deref(), Some("jump to character"));
        press(&mut vim, "a");
        assert_eq!(vim.jump_hint(at(1, 2)), Some("a"));
        assert_eq!(vim.jump_hint(at(1, 6)), Some("s"));
        press(&mut vim, "s");
        assert_eq!(vim.cursor, at(1, 6));
        assert_eq!(vim.jump_hint(at(1, 2)), None);

        // Entering the mode on a jump is already asking for the character.
        let mut vim = sample_vim();
        vim.start_jump();
        assert_eq!(vim.prompt().as_deref(), Some("jump to character"));
        press(&mut vim, "a s");
        assert_eq!(vim.cursor, at(1, 6));
    }

    #[test]
    fn jump_hints_alternate_forward_and_backward_from_the_cursor() {
        let mut vim = mode(vec!["a.a.a".into()], at(0, 2), 1);
        press(&mut vim, " a");
        assert_eq!(vim.jump_hint(at(0, 4)), Some("a"));
        assert_eq!(vim.jump_hint(at(0, 0)), Some("s"));
    }

    #[test]
    fn jump_hints_use_two_keys_after_single_keys_run_out() {
        let mut vim = mode(vec!["a".repeat(30)], at(0, 0), 1);
        press(&mut vim, " a");
        assert_eq!(vim.jump_hint(at(0, 1)), Some("a"));
        assert_eq!(vim.jump_hint(at(0, 26)), Some("ja"));
        press(&mut vim, "j");
        assert_eq!(vim.jump_hint(at(0, 26)), Some("a"));
        assert_eq!(vim.jump_hint(at(0, 27)), Some("s"));
        press(&mut vim, "s");
        assert_eq!(vim.cursor, at(0, 27));
    }

    #[test]
    fn forward_backward_search_and_repeats_wrap() {
        let mut vim = sample_vim();
        press(&mut vim, "/");
        type_text(&mut vim, "alpha");
        press(&mut vim, "Enter");
        assert_eq!(vim.cursor, at(1, 2));
        press(&mut vim, "n");
        assert_eq!(vim.cursor, at(1, 13));
        press(&mut vim, "N");
        assert_eq!(vim.cursor, at(1, 2));

        press(&mut vim, "?");
        type_text(&mut vim, "one");
        press(&mut vim, "Enter");
        assert_eq!(vim.cursor, at(0, 0));
    }

    #[test]
    fn searching_highlights_every_match_while_the_pattern_is_typed() {
        let mut vim = sample_vim();
        press(&mut vim, "/");
        type_text(&mut vim, "alpha");
        // The highlight follows the prompt, before Enter accepts it.
        assert!(vim.search_match(at(1, 2)));
        assert!(vim.search_match(at(1, 6)));
        assert!(vim.search_match(at(1, 13)));
        assert!(!vim.search_match(at(1, 7)));
        assert!(!vim.search_match(at(0, 0)));

        // Backspacing shortens every highlight with the pattern.
        press(&mut vim, "Backspace");
        assert!(vim.search_match(at(1, 5)));
        assert!(!vim.search_match(at(1, 6)));

        press(&mut vim, "a Enter");
        assert_eq!(vim.cursor, at(1, 2));
        assert!(vim.current_search_match(at(1, 2)));
        assert!(vim.current_search_match(at(1, 6)));
        // Other matches stay highlighted, but only one is the current one.
        assert!(vim.search_match(at(1, 13)));
        assert!(!vim.current_search_match(at(1, 13)));

        press(&mut vim, "n");
        assert!(vim.current_search_match(at(1, 13)));
        assert!(!vim.current_search_match(at(1, 2)));

        // Escape clears the highlight without leaving the mode.
        assert!(matches!(press(&mut vim, "Escape"), VimOutcome::None));
        assert!(!vim.search_match(at(1, 2)));
        assert!(matches!(press(&mut vim, "Escape"), VimOutcome::Exit));
    }

    #[test]
    fn a_cancelled_search_leaves_nothing_highlighted() {
        let mut vim = sample_vim();
        press(&mut vim, "/ a");
        assert!(vim.search_match(at(1, 2)));
        press(&mut vim, "Escape");
        assert!(!vim.search_match(at(1, 2)));
        assert_eq!(vim.cursor, at(0, 0));
    }

    #[test]
    fn character_line_and_block_selections_yank() {
        let mut vim = sample_vim();
        assert_eq!(press(&mut vim, "v e y").yank(), "one");

        let mut vim = sample_vim();
        assert_eq!(
            press(&mut vim, "V j y").yank(),
            "one two-three\n  alpha beta alpha\n"
        );

        let mut vim = mode(vec!["abcd".into(), "efgh".into()], at(0, 1), 2);
        assert_eq!(press(&mut vim, "Ctrl-v l j y").yank(), "bc\nfg");
    }

    #[test]
    fn yank_with_motion_and_escape_selection_rule() {
        for (keys, expected) in [
            ("y e", "one"),
            ("y f -", "one two-"),
            ("G y g g", "one two-three\n  alpha beta alpha\nlast line\n"),
            ("2 y y", "one two-three\n  alpha beta alpha\n"),
            ("w Y", "two-three"),
            ("w 2 Y", "two-three\n  alpha beta alpha"),
        ] {
            assert_eq!(press(&mut sample_vim(), keys).yank(), expected, "{keys}");
        }

        let mut vim = sample_vim();
        assert!(matches!(press(&mut vim, "v Escape"), VimOutcome::None));
        assert!(matches!(press(&mut vim, "Escape"), VimOutcome::Exit));
    }
}
