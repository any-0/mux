use std::{
    cmp::{max, min},
    collections::HashMap,
    ops::Range,
};

use crate::{
    config::Action,
    protocol::{Key, KeyCode},
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Position {
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionKind {
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
    },
    Find {
        forward: bool,
        till: bool,
        count: usize,
    },
    YankFind {
        forward: bool,
        till: bool,
        count: usize,
        start: Position,
    },
    Search {
        forward: bool,
        query: String,
        count: usize,
    },
    Yank {
        count: usize,
    },
    YankGoTop {
        count: usize,
        start: Position,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug)]
pub enum VimOutcome {
    None,
    Exit,
    Yank(String),
}

#[derive(Clone, Debug)]
pub struct VimMode {
    lines: Vec<String>,
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
    /// Length of the highlighted pattern, used to mark the match under the
    /// cursor.
    search_length: usize,
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
    pub fn new(lines: Vec<String>, cursor: Position, viewport_height: usize) -> Self {
        let lines = if lines.is_empty() {
            vec![String::new()]
        } else {
            lines
        };
        let mut mode = Self {
            lines,
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
            search_length: 0,
            jump_list: Vec::new(),
            jump_index: 0,
        };
        mode.cursor = mode.clamp(mode.cursor);
        mode.viewport_top = mode.lines.len().saturating_sub(mode.viewport_height);
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
            && self.search_match(position)
            && self
                .search_matches
                .get(&position.row)
                .is_some_and(|ranges| {
                    ranges.iter().any(|range| {
                        range.start == self.cursor.col && range.contains(&position.col)
                    })
                })
    }

    /// Indexes every match of `query` so the renderer can highlight them.
    fn highlight_search(&mut self, query: &str) {
        self.search_matches.clear();
        self.search_length = query.chars().count();
        if query.is_empty() {
            return;
        }
        for (row, line) in self.lines.iter().enumerate() {
            let mut byte_start = 0;
            while let Some(offset) = line[byte_start..].find(query) {
                let byte = byte_start + offset;
                let start = line[..byte].chars().count();
                self.search_matches
                    .entry(row)
                    .or_default()
                    .push(start..start + self.search_length);
                byte_start = byte + query.len();
            }
        }
    }

    fn clear_search_highlight(&mut self) {
        self.search_matches.clear();
        self.search_length = 0;
    }

    pub fn selected(&self, position: Position) -> bool {
        let Some(selection) = &self.selection else {
            return false;
        };
        match selection.kind {
            SelectionKind::Character => {
                let start = min(selection.anchor, self.cursor);
                let end = max(selection.anchor, self.cursor);
                position >= start && position <= end
            }
            SelectionKind::Line => {
                let start = min(selection.anchor.row, self.cursor.row);
                let end = max(selection.anchor.row, self.cursor.row);
                (start..=end).contains(&position.row)
            }
            SelectionKind::Block => {
                let rows = min(selection.anchor.row, self.cursor.row)
                    ..=max(selection.anchor.row, self.cursor.row);
                let cols = min(selection.anchor.col, self.cursor.col)
                    ..=max(selection.anchor.col, self.cursor.col);
                rows.contains(&position.row) && cols.contains(&position.col)
            }
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

        if matches!(self.pending, Pending::Search { .. }) {
            return self.handle_search_prompt(key);
        }
        if matches!(
            self.pending,
            Pending::JumpCharacter | Pending::JumpTarget { .. }
        ) {
            return self.handle_jump(key);
        }
        if matches!(
            self.pending,
            Pending::Find { .. } | Pending::YankFind { .. }
        ) {
            return self.handle_find_character(key);
        }

        let count_digit = match key {
            Key {
                code: KeyCode::Char(digit @ '0'..='9'),
                modifiers: 0,
            } if *digit != '0' || self.count.is_some() => digit.to_digit(10),
            _ => None,
        };
        if let Some(digit) = count_digit {
            self.count = Some(
                self.count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit as usize),
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
        } = self.pending.clone()
        {
            if action == Action::Yank {
                let count = operator_count.saturating_mul(self.take_count());
                return VimOutcome::Yank(self.yank_lines(self.cursor.row, count));
            }
            let motion_count = operator_count.saturating_mul(self.take_count());
            if matches!(
                action,
                Action::FindForward
                    | Action::FindBackward
                    | Action::TillForward
                    | Action::TillBackward
            ) {
                self.pending = Pending::YankFind {
                    forward: matches!(action, Action::FindForward | Action::TillForward),
                    till: matches!(action, Action::TillForward | Action::TillBackward),
                    count: motion_count,
                    start: self.cursor,
                };
                return VimOutcome::None;
            }
            if action == Action::GoTop {
                self.pending = Pending::YankGoTop {
                    count: motion_count,
                    start: self.cursor,
                };
                return VimOutcome::None;
            }
            if let Some(motion) = self.motion(action, motion_count) {
                self.pending = Pending::None;
                return VimOutcome::Yank(self.yank_motion(self.cursor, motion));
            }
            self.pending = Pending::None;
            return VimOutcome::None;
        }

        if let Pending::YankGoTop { count, start } = self.pending.clone() {
            self.pending = Pending::None;
            if action == Action::GoTop {
                let row = if count == 1 {
                    0
                } else {
                    count.saturating_sub(1).min(self.lines.len() - 1)
                };
                return VimOutcome::Yank(self.yank_motion(
                    start,
                    Motion {
                        destination: Position {
                            row,
                            col: self.first_nonblank(row),
                        },
                        inclusive: false,
                        linewise: true,
                    },
                ));
            }
            return VimOutcome::None;
        }

        if let Pending::GoTop { count } = self.pending.clone() {
            self.pending = Pending::None;
            if action == Action::GoTop {
                let start = self.cursor;
                let row = if count == 1 {
                    0
                } else {
                    count.saturating_sub(1).min(self.lines.len() - 1)
                };
                self.move_cursor(Motion {
                    destination: Position {
                        row,
                        col: self.first_nonblank(row),
                    },
                    inclusive: false,
                    linewise: true,
                });
                self.record_jump(start);
                return VimOutcome::None;
            }
        }

        match action {
            Action::Escape => {
                self.count = None;
                self.pending = Pending::None;
                let highlighted = !self.search_matches.is_empty();
                self.clear_search_highlight();
                if self.selection.take().is_some() || highlighted {
                    VimOutcome::None
                } else {
                    VimOutcome::Exit
                }
            }
            Action::Visual => {
                self.toggle_selection(SelectionKind::Character);
                VimOutcome::None
            }
            Action::VisualLine => {
                self.toggle_selection(SelectionKind::Line);
                VimOutcome::None
            }
            Action::VisualBlock => {
                self.toggle_selection(SelectionKind::Block);
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
                    .min(self.lines.len() - 1);
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
                self.pending = Pending::GoTop { count };
                VimOutcome::None
            }
            Action::FindForward
            | Action::FindBackward
            | Action::TillForward
            | Action::TillBackward => {
                let count = self.take_count();
                self.pending = Pending::Find {
                    forward: matches!(action, Action::FindForward | Action::TillForward),
                    till: matches!(action, Action::TillForward | Action::TillBackward),
                    count,
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
                    let forward = if action == Action::RepeatSearch {
                        search.forward
                    } else {
                        !search.forward
                    };
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
                        .min(self.lines.len().saturating_sub(self.viewport_height));
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
                    .min(self.lines.len().saturating_sub(self.viewport_height));
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
        if matches!(key.code, KeyCode::Escape) {
            self.pending = Pending::None;
            return VimOutcome::None;
        }

        let Key {
            code: KeyCode::Char(character),
            modifiers: 0,
        } = key
        else {
            self.pending = Pending::None;
            return VimOutcome::None;
        };

        let pending = std::mem::replace(&mut self.pending, Pending::None);
        match pending {
            Pending::JumpCharacter => {
                let targets = self.jump_targets(*character);
                if targets.is_empty() {
                    self.message = Some(format!("character {:?} not visible", character));
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

    fn jump_targets(&self, character: char) -> Vec<JumpTarget> {
        let visible_end = min(
            self.viewport_top.saturating_add(self.viewport_height),
            self.lines.len(),
        );
        let mut forward = Vec::new();
        let mut backward = Vec::new();
        for row in self.viewport_top..visible_end {
            for (col, candidate) in self.lines[row].chars().enumerate() {
                if candidate != character {
                    continue;
                }
                let position = Position { row, col };
                if position > self.cursor {
                    forward.push(position);
                } else if position < self.cursor {
                    backward.push(position);
                }
            }
        }
        backward.reverse();

        let mut positions = Vec::with_capacity(forward.len() + backward.len());
        for index in 0..max(forward.len(), backward.len()) {
            if let Some(position) = forward.get(index) {
                positions.push(*position);
            }
            if let Some(position) = backward.get(index) {
                positions.push(*position);
            }
        }
        let mut targets = Vec::with_capacity(positions.len());
        assign_jump_labels(&positions, "", &mut targets);
        targets
    }

    fn handle_search_prompt(&mut self, key: &Key) -> VimOutcome {
        match key {
            Key {
                code: KeyCode::Escape,
                ..
            } => {
                self.pending = Pending::None;
                self.clear_search_highlight();
            }
            Key {
                code: KeyCode::Backspace,
                ..
            } => {
                if let Pending::Search { query, .. } = &mut self.pending {
                    query.pop();
                    let query = query.clone();
                    self.highlight_search(&query);
                }
            }
            Key {
                code: KeyCode::Enter,
                ..
            } => {
                let pending = std::mem::replace(&mut self.pending, Pending::None);
                if let Pending::Search {
                    forward,
                    query,
                    count,
                } = pending
                    && !query.is_empty()
                {
                    let start = self.cursor;
                    self.last_search = Some(Search {
                        query: query.clone(),
                        forward,
                    });
                    self.apply_search(&query, forward, count);
                    self.record_jump(start);
                }
            }
            Key {
                code: KeyCode::Char(character),
                modifiers: 0,
                ..
            } => {
                if let Pending::Search { query, .. } = &mut self.pending {
                    query.push(*character);
                    let query = query.clone();
                    self.highlight_search(&query);
                }
            }
            _ => {}
        }
        VimOutcome::None
    }

    fn handle_find_character(&mut self, key: &Key) -> VimOutcome {
        let pending = std::mem::replace(&mut self.pending, Pending::None);
        let Key {
            code: KeyCode::Char(character),
            modifiers: 0,
        } = key
        else {
            return VimOutcome::None;
        };
        match pending {
            Pending::Find {
                forward,
                till,
                count,
            } => {
                let find = Find {
                    character: *character,
                    forward,
                    till,
                };
                self.last_find = Some(find);
                self.apply_find(find, count);
                VimOutcome::None
            }
            Pending::YankFind {
                forward,
                till,
                count,
                start,
            } => {
                let find = Find {
                    character: *character,
                    forward,
                    till,
                };
                self.last_find = Some(find);
                if self.apply_find(find, count) {
                    VimOutcome::Yank(self.yank_motion(
                        start,
                        Motion {
                            destination: self.cursor,
                            inclusive: true,
                            linewise: false,
                        },
                    ))
                } else {
                    VimOutcome::None
                }
            }
            _ => VimOutcome::None,
        }
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1)
    }

    fn toggle_selection(&mut self, kind: SelectionKind) {
        match &mut self.selection {
            Some(selection) if selection.kind == kind => {
                self.selection = None;
            }
            Some(selection) => {
                selection.kind = kind;
            }
            None => {
                self.selection = Some(Selection {
                    anchor: self.cursor,
                    kind,
                });
            }
        }
    }

    /// Scrolls the viewport by `lines`, keeping the cursor on screen.
    ///
    /// Reports whether it moved: at the bottom of the buffer there is nothing
    /// left to scroll to, which is the caller's cue to leave vim mode.
    pub fn scroll(&mut self, up: bool, lines: usize) -> bool {
        let lowest = self.lines.len().saturating_sub(self.viewport_height);
        let target = if up {
            self.viewport_top.saturating_sub(lines)
        } else {
            self.viewport_top.saturating_add(lines).min(lowest)
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
            .min(self.lines.len().saturating_sub(1));
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
        if self.jump_list.get(self.jump_index).copied() != Some(start) {
            self.jump_list.push(start);
        }
        if self.jump_list.last().copied() != Some(self.cursor) {
            self.jump_list.push(self.cursor);
        }
        if self.jump_list.len() > JUMP_LIST_CAPACITY {
            let excess = self.jump_list.len() - JUMP_LIST_CAPACITY;
            self.jump_list.drain(..excess);
        }
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
        self.viewport_top = self
            .viewport_top
            .min(self.lines.len().saturating_sub(self.viewport_height));
    }

    fn clamp(&self, mut position: Position) -> Position {
        position.row = position.row.min(self.lines.len() - 1);
        position.col = position.col.min(self.line_end(position.row));
        position
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn line_end(&self, row: usize) -> usize {
        self.line_len(row).saturating_sub(1)
    }

    fn first_nonblank(&self, row: usize) -> usize {
        self.lines[row]
            .chars()
            .position(|character| !character.is_whitespace())
            .unwrap_or(0)
    }

    fn motion(&self, action: Action, count: usize) -> Option<Motion> {
        let mut position = self.cursor;
        let count = count.max(1);
        let (inclusive, linewise) = match action {
            Action::CursorLeft => {
                position.col = position.col.saturating_sub(count);
                (true, false)
            }
            Action::CursorRight => {
                position.col = min(
                    position.col.saturating_add(count),
                    self.line_end(position.row),
                );
                (true, false)
            }
            Action::CursorDown => {
                position.row = min(position.row.saturating_add(count), self.lines.len() - 1);
                (false, true)
            }
            Action::CursorUp => {
                position.row = position.row.saturating_sub(count);
                (false, true)
            }
            Action::CursorDown3 => {
                position.row = min(
                    position.row.saturating_add(count.saturating_mul(3)),
                    self.lines.len() - 1,
                );
                (false, true)
            }
            Action::CursorUp3 => {
                position.row = position.row.saturating_sub(count.saturating_mul(3));
                (false, true)
            }
            Action::CursorDown10 => {
                position.row = min(
                    position.row.saturating_add(count.saturating_mul(10)),
                    self.lines.len() - 1,
                );
                (false, true)
            }
            Action::CursorUp10 => {
                position.row = position.row.saturating_sub(count.saturating_mul(10));
                (false, true)
            }
            Action::HalfPageDown | Action::HalfPageDownCenter => {
                position.row = min(
                    position
                        .row
                        .saturating_add((self.viewport_height / 2).max(1).saturating_mul(count)),
                    self.lines.len() - 1,
                );
                (false, true)
            }
            Action::HalfPageUp | Action::HalfPageUpCenter => {
                position.row = position
                    .row
                    .saturating_sub((self.viewport_height / 2).max(1).saturating_mul(count));
                (false, true)
            }
            Action::WordForward => {
                position = self.word_forward(position, count, false);
                (false, false)
            }
            Action::BigWordForward => {
                position = self.word_forward(position, count, true);
                (false, false)
            }
            Action::WordEnd => {
                position = self.word_end(position, count, false);
                (true, false)
            }
            Action::BigWordEnd => {
                position = self.word_end(position, count, true);
                (true, false)
            }
            Action::WordBackward => {
                position = self.word_backward(position, count, false);
                (false, false)
            }
            Action::BigWordBackward => {
                position = self.word_backward(position, count, true);
                (false, false)
            }
            Action::LineStart => {
                position.col = 0;
                (false, false)
            }
            Action::FirstNonBlank => {
                position.col = self.first_nonblank(position.row);
                (false, false)
            }
            Action::LineEnd => {
                position.col = self.line_end(position.row);
                (true, false)
            }
            Action::GoBottom => {
                position.row = if count == 1 {
                    self.lines.len() - 1
                } else {
                    count.saturating_sub(1).min(self.lines.len() - 1)
                };
                position.col = self.first_nonblank(position.row);
                (false, true)
            }
            _ => return None,
        };
        position = self.clamp(position);
        Some(Motion {
            destination: position,
            inclusive,
            linewise,
        })
    }

    fn flat(&self) -> Vec<char> {
        let mut result = Vec::new();
        for (index, line) in self.lines.iter().enumerate() {
            result.extend(line.chars());
            if index + 1 < self.lines.len() {
                result.push('\n');
            }
        }
        result
    }

    fn position_index(&self, position: Position) -> usize {
        self.lines
            .iter()
            .take(position.row)
            .map(|line| line.chars().count() + 1)
            .sum::<usize>()
            + position.col.min(self.line_len(position.row))
    }

    fn index_position(&self, index: usize) -> Position {
        let mut remaining = index;
        for (row, line) in self.lines.iter().enumerate() {
            let len = line.chars().count();
            if remaining < len {
                return Position {
                    row,
                    col: remaining,
                };
            }
            if remaining == len {
                if row + 1 < self.lines.len() {
                    return Position {
                        row: row + 1,
                        col: 0,
                    };
                }
                return Position {
                    row,
                    col: len.saturating_sub(1),
                };
            }
            remaining = remaining.saturating_sub(len + 1);
        }
        Position {
            row: self.lines.len() - 1,
            col: self.line_end(self.lines.len() - 1),
        }
    }

    fn category(character: char, big: bool) -> u8 {
        if character.is_whitespace() {
            0
        } else if big || character.is_alphanumeric() || character == '_' {
            1
        } else {
            2
        }
    }

    fn word_forward(&self, start: Position, count: usize, big: bool) -> Position {
        let text = self.flat();
        if text.is_empty() {
            return start;
        }
        let mut index = self.position_index(start).min(text.len().saturating_sub(1));
        for _ in 0..count {
            let category = Self::category(text[index], big);
            if category != 0 {
                while index + 1 < text.len() && Self::category(text[index + 1], big) == category {
                    index += 1;
                }
                if index + 1 < text.len() {
                    index += 1;
                }
            }
            while index + 1 < text.len() && Self::category(text[index], big) == 0 {
                index += 1;
            }
        }
        self.index_position(index)
    }

    fn word_end(&self, start: Position, count: usize, big: bool) -> Position {
        let text = self.flat();
        if text.is_empty() {
            return start;
        }
        let mut index = self.position_index(start).min(text.len().saturating_sub(1));
        for iteration in 0..count {
            if iteration > 0 && index + 1 < text.len() {
                index += 1;
            }
            while index + 1 < text.len() && Self::category(text[index], big) == 0 {
                index += 1;
            }
            if index + 1 < text.len()
                && Self::category(text[index], big) != 0
                && iteration == 0
                && Self::category(text[index + 1], big) == Self::category(text[index], big)
            {
                // remain in the current word
            } else if index + 1 < text.len()
                && Self::category(text[index], big) != 0
                && iteration == 0
                && Self::category(text[index + 1], big) != Self::category(text[index], big)
            {
                index += 1;
                while index + 1 < text.len() && Self::category(text[index], big) == 0 {
                    index += 1;
                }
            }
            let category = Self::category(text[index], big);
            while index + 1 < text.len()
                && category != 0
                && Self::category(text[index + 1], big) == category
            {
                index += 1;
            }
        }
        self.index_position(index)
    }

    fn word_backward(&self, start: Position, count: usize, big: bool) -> Position {
        let text = self.flat();
        if text.is_empty() {
            return start;
        }
        let mut index = self.position_index(start).min(text.len().saturating_sub(1));
        for _ in 0..count {
            index = index.saturating_sub(1);
            while index > 0 && Self::category(text[index], big) == 0 {
                index -= 1;
            }
            let category = Self::category(text[index], big);
            while index > 0 && Self::category(text[index - 1], big) == category {
                index -= 1;
            }
        }
        self.index_position(index)
    }

    fn apply_find(&mut self, find: Find, count: usize) -> bool {
        let characters: Vec<_> = self.lines[self.cursor.row].chars().collect();
        let found = if find.forward {
            ((self.cursor.col + 1)..characters.len())
                .filter(|index| characters[*index] == find.character)
                .nth(count - 1)
        } else {
            (0..self.cursor.col)
                .rev()
                .filter(|index| characters[*index] == find.character)
                .nth(count - 1)
        };
        if let Some(mut col) = found {
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
        } else {
            self.message = Some(format!("character {:?} not found", find.character));
            false
        }
    }

    fn apply_search(&mut self, query: &str, forward: bool, count: usize) {
        self.highlight_search(query);
        let mut matches = Vec::new();
        for (row, line) in self.lines.iter().enumerate() {
            let mut byte_start = 0;
            while let Some(offset) = line[byte_start..].find(query) {
                let byte = byte_start + offset;
                matches.push(Position {
                    row,
                    col: line[..byte].chars().count(),
                });
                byte_start = byte + query.len();
            }
        }
        if matches.is_empty() {
            self.message = Some(format!("pattern not found: {query}"));
            return;
        }
        let mut current = self.cursor;
        for _ in 0..count {
            current = if forward {
                matches
                    .iter()
                    .copied()
                    .find(|position| *position > current)
                    .unwrap_or(matches[0])
            } else {
                matches
                    .iter()
                    .rev()
                    .copied()
                    .find(|position| *position < current)
                    .unwrap_or(*matches.last().unwrap())
            };
        }
        self.cursor = current;
        self.ensure_visible();
    }

    fn yank_selection(&self) -> String {
        let selection = self.selection.as_ref().unwrap();
        match selection.kind {
            SelectionKind::Character => self.text_between(selection.anchor, self.cursor, true),
            SelectionKind::Line => {
                let start = min(selection.anchor.row, self.cursor.row);
                self.yank_lines(
                    start,
                    max(selection.anchor.row, self.cursor.row) - start + 1,
                )
            }
            SelectionKind::Block => {
                let start_row = min(selection.anchor.row, self.cursor.row);
                let end_row = max(selection.anchor.row, self.cursor.row);
                let start_col = min(selection.anchor.col, self.cursor.col);
                let end_col = max(selection.anchor.col, self.cursor.col);
                (start_row..=end_row)
                    .map(|row| {
                        self.lines[row]
                            .chars()
                            .skip(start_col)
                            .take(end_col - start_col + 1)
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }

    fn yank_lines(&self, start: usize, count: usize) -> String {
        let end = min(start.saturating_add(count), self.lines.len());
        let mut text = self.lines[start..end].join("\n");
        text.push('\n');
        text
    }

    fn yank_motion(&self, start: Position, motion: Motion) -> String {
        if motion.linewise {
            let first = min(start.row, motion.destination.row);
            return self.yank_lines(first, max(start.row, motion.destination.row) - first + 1);
        }
        self.text_between(start, motion.destination, motion.inclusive)
    }

    fn text_between(&self, a: Position, b: Position, inclusive: bool) -> String {
        let flat = self.flat();
        let a = self.position_index(a);
        let b = self.position_index(b);
        let (start, mut end) = if a <= b { (a, b) } else { (b, a) };
        if inclusive {
            end = end.saturating_add(1);
        }
        flat[start.min(flat.len())..end.min(flat.len())]
            .iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Bindings, config::Mode, protocol::parse_for_test};

    fn sample_vim() -> VimMode {
        VimMode::new(
            vec![
                "one two-three".into(),
                "  alpha beta alpha".into(),
                "last line".into(),
            ],
            Position { row: 0, col: 0 },
            2,
        )
    }

    fn press(vim: &mut VimMode, name: &str) -> VimOutcome {
        let key = parse_for_test(name);
        let action = Bindings::defaults().get(Mode::Vim, &key);
        vim.handle(action, &key)
    }

    #[test]
    fn counts_and_word_motions() {
        let mut vim = sample_vim();
        press(&mut vim, "2");
        press(&mut vim, "w");
        assert_eq!(vim.cursor, Position { row: 0, col: 7 });
        press(&mut vim, "b");
        assert_eq!(vim.cursor, Position { row: 0, col: 4 });
        press(&mut vim, "e");
        assert_eq!(vim.cursor, Position { row: 0, col: 6 });
    }

    #[test]
    fn jumps_move_backward_and_forward_with_counts() {
        let lines = (0..30)
            .map(|number| format!("line {number}"))
            .collect::<Vec<_>>();
        let mut vim = VimMode::new(lines, Position { row: 20, col: 0 }, 6);

        press(&mut vim, "Ctrl-u");
        assert_eq!(vim.cursor.row, 17);
        press(&mut vim, "G");
        assert_eq!(vim.cursor.row, 29);

        press(&mut vim, "2");
        press(&mut vim, "Ctrl-o");
        assert_eq!(vim.cursor.row, 20);
        press(&mut vim, "Ctrl-l");
        assert_eq!(vim.cursor.row, 17);
        press(&mut vim, "Tab");
        assert_eq!(vim.cursor.row, 29);
    }

    #[test]
    fn a_new_jump_discards_newer_jump_positions() {
        let lines = (0..30)
            .map(|number| format!("line {number}"))
            .collect::<Vec<_>>();
        let mut vim = VimMode::new(lines, Position { row: 20, col: 0 }, 6);

        press(&mut vim, "Ctrl-u");
        press(&mut vim, "G");
        press(&mut vim, "Ctrl-o");
        assert_eq!(vim.cursor.row, 17);

        press(&mut vim, "g");
        press(&mut vim, "g");
        assert_eq!(vim.cursor.row, 0);
        press(&mut vim, "Ctrl-l");
        assert_eq!(vim.cursor.row, 0);
        press(&mut vim, "Ctrl-o");
        assert_eq!(vim.cursor.row, 17);
    }

    #[test]
    fn ordinary_motions_do_not_enter_the_jump_list() {
        let mut vim = sample_vim();
        press(&mut vim, "j");
        press(&mut vim, "w");
        let position = vim.cursor;
        press(&mut vim, "Ctrl-o");
        assert_eq!(vim.cursor, position);
    }

    #[test]
    fn the_wheel_scrolls_the_viewport_and_stops_at_the_bottom() {
        let lines = (0..20)
            .map(|number| format!("line {number}"))
            .collect::<Vec<_>>();
        let mut vim = VimMode::new(lines, Position { row: 19, col: 0 }, 6);
        // Entering vim mode starts at the end of the buffer.
        assert_eq!(vim.viewport_top, 14);
        assert!(vim.scroll(true, 3));
        assert_eq!(vim.viewport_top, 11);
        assert!(
            vim.cursor.row < 11 + 6 && vim.cursor.row >= 11,
            "the cursor came along with the view"
        );
        assert!(vim.scroll(false, 3));
        assert_eq!(vim.viewport_top, 14);
        // Already at the bottom: nothing to scroll to, which ends vim mode.
        assert!(!vim.scroll(false, 3));
        while vim.scroll(true, 5) {}
        assert_eq!(vim.viewport_top, 0);
        assert!(!vim.scroll(true, 5));
    }

    #[test]
    fn line_vertical_page_and_file_motions() {
        let lines = (0..20)
            .map(|number| format!("  line {number}"))
            .collect::<Vec<_>>();
        let mut vim = VimMode::new(lines, Position { row: 10, col: 4 }, 6);
        press(&mut vim, "0");
        assert_eq!(vim.cursor.col, 0);
        press(&mut vim, "^");
        assert_eq!(vim.cursor.col, 2);
        press(&mut vim, "$");
        assert_eq!(vim.cursor.col, 8);
        press(&mut vim, "2");
        press(&mut vim, "k");
        assert_eq!(vim.cursor.row, 8);
        press(&mut vim, "Ctrl-u");
        assert_eq!(vim.cursor.row, 5);
        press(&mut vim, "G");
        assert_eq!(vim.cursor, Position { row: 19, col: 2 });
        press(&mut vim, "g");
        press(&mut vim, "g");
        assert_eq!(vim.cursor, Position { row: 0, col: 2 });
        press(&mut vim, "5");
        press(&mut vim, "G");
        assert_eq!(vim.cursor, Position { row: 4, col: 2 });
    }

    #[test]
    fn arrow_keys_follow_basic_motions() {
        let mut vim = sample_vim();
        press(&mut vim, "Right");
        assert_eq!(vim.cursor, Position { row: 0, col: 1 });
        press(&mut vim, "Down");
        assert_eq!(vim.cursor, Position { row: 1, col: 1 });
        press(&mut vim, "Left");
        assert_eq!(vim.cursor, Position { row: 1, col: 0 });
        press(&mut vim, "Up");
        assert_eq!(vim.cursor, Position { row: 0, col: 0 });
    }

    #[test]
    fn word_and_big_word_classes_differ() {
        let mut vim = VimMode::new(vec!["one-two three".into()], Position { row: 0, col: 0 }, 1);
        press(&mut vim, "w");
        assert_eq!(vim.cursor.col, 3);
        press(&mut vim, "0");
        press(&mut vim, "W");
        assert_eq!(vim.cursor.col, 8);
        press(&mut vim, "B");
        assert_eq!(vim.cursor.col, 0);
        press(&mut vim, "E");
        assert_eq!(vim.cursor.col, 6);
    }

    #[test]
    fn find_till_and_swappable_repeats() {
        let mut vim = sample_vim();
        press(&mut vim, "f");
        press(&mut vim, "-");
        assert_eq!(vim.cursor.col, 7);
        press(&mut vim, ",");
        assert_eq!(vim.cursor.col, 7);
        press(&mut vim, "t");
        press(&mut vim, "e");
        assert_eq!(vim.cursor.col, 10);
    }

    #[test]
    fn space_labels_visible_character_matches_and_jumps_by_hint() {
        let mut vim = sample_vim();
        press(&mut vim, " ");
        assert_eq!(vim.prompt().as_deref(), Some("jump to character"));
        press(&mut vim, "a");
        assert_eq!(vim.jump_hint(Position { row: 1, col: 2 }), Some("a"));
        assert_eq!(vim.jump_hint(Position { row: 1, col: 6 }), Some("s"));
        press(&mut vim, "s");
        assert_eq!(vim.cursor, Position { row: 1, col: 6 });
        assert_eq!(vim.jump_hint(Position { row: 1, col: 2 }), None);
    }

    #[test]
    fn a_mode_started_on_a_jump_is_already_asking_for_the_character() {
        let mut vim = sample_vim();
        vim.start_jump();
        assert_eq!(vim.prompt().as_deref(), Some("jump to character"));
        press(&mut vim, "a");
        press(&mut vim, "s");
        assert_eq!(vim.cursor, Position { row: 1, col: 6 });
    }

    #[test]
    fn jump_hints_alternate_forward_and_backward_from_the_cursor() {
        let mut vim = VimMode::new(vec!["a.a.a".into()], Position { row: 0, col: 2 }, 1);
        press(&mut vim, " ");
        press(&mut vim, "a");
        assert_eq!(vim.jump_hint(Position { row: 0, col: 4 }), Some("a"));
        assert_eq!(vim.jump_hint(Position { row: 0, col: 0 }), Some("s"));
    }

    #[test]
    fn jump_hints_use_two_keys_after_single_keys_run_out() {
        let mut vim = VimMode::new(vec!["a".repeat(30)], Position { row: 0, col: 0 }, 1);
        press(&mut vim, " ");
        press(&mut vim, "a");

        assert_eq!(vim.jump_hint(Position { row: 0, col: 1 }), Some("a"));
        assert_eq!(vim.jump_hint(Position { row: 0, col: 26 }), Some("ja"));
        press(&mut vim, "j");
        assert_eq!(vim.jump_hint(Position { row: 0, col: 26 }), Some("a"));
        assert_eq!(vim.jump_hint(Position { row: 0, col: 27 }), Some("s"));
        press(&mut vim, "s");
        assert_eq!(vim.cursor, Position { row: 0, col: 27 });
    }

    #[test]
    fn forward_backward_search_and_repeats_wrap() {
        let mut vim = sample_vim();
        press(&mut vim, "/");
        for character in "alpha".chars() {
            press(&mut vim, &character.to_string());
        }
        press(&mut vim, "Enter");
        assert_eq!(vim.cursor, Position { row: 1, col: 2 });
        press(&mut vim, "n");
        assert_eq!(vim.cursor, Position { row: 1, col: 13 });
        press(&mut vim, "N");
        assert_eq!(vim.cursor, Position { row: 1, col: 2 });

        press(&mut vim, "?");
        for character in "one".chars() {
            press(&mut vim, &character.to_string());
        }
        press(&mut vim, "Enter");
        assert_eq!(vim.cursor, Position { row: 0, col: 0 });
    }

    #[test]
    fn searching_highlights_every_match_while_the_pattern_is_typed() {
        let mut vim = sample_vim();
        press(&mut vim, "/");
        for character in "alpha".chars() {
            press(&mut vim, &character.to_string());
        }
        // The highlight follows the prompt, before Enter accepts it.
        assert!(vim.search_match(Position { row: 1, col: 2 }));
        assert!(vim.search_match(Position { row: 1, col: 6 }));
        assert!(vim.search_match(Position { row: 1, col: 13 }));
        assert!(!vim.search_match(Position { row: 1, col: 7 }));
        assert!(!vim.search_match(Position { row: 0, col: 0 }));

        // Backspacing shortens every highlight with the pattern.
        press(&mut vim, "Backspace");
        assert!(vim.search_match(Position { row: 1, col: 5 }));
        assert!(!vim.search_match(Position { row: 1, col: 6 }));

        press(&mut vim, "a");
        press(&mut vim, "Enter");
        assert_eq!(vim.cursor, Position { row: 1, col: 2 });
        assert!(vim.current_search_match(Position { row: 1, col: 2 }));
        assert!(vim.current_search_match(Position { row: 1, col: 6 }));
        // Other matches stay highlighted, but only one is the current one.
        assert!(vim.search_match(Position { row: 1, col: 13 }));
        assert!(!vim.current_search_match(Position { row: 1, col: 13 }));

        press(&mut vim, "n");
        assert!(vim.current_search_match(Position { row: 1, col: 13 }));
        assert!(!vim.current_search_match(Position { row: 1, col: 2 }));

        // Escape clears the highlight without leaving the mode.
        assert!(matches!(press(&mut vim, "Escape"), VimOutcome::None));
        assert!(!vim.search_match(Position { row: 1, col: 2 }));
        assert!(matches!(press(&mut vim, "Escape"), VimOutcome::Exit));
    }

    #[test]
    fn a_cancelled_search_leaves_nothing_highlighted() {
        let mut vim = sample_vim();
        press(&mut vim, "/");
        press(&mut vim, "a");
        assert!(vim.search_match(Position { row: 1, col: 2 }));
        press(&mut vim, "Escape");
        assert!(!vim.search_match(Position { row: 1, col: 2 }));
        assert_eq!(vim.cursor, Position { row: 0, col: 0 });
    }

    #[test]
    fn character_line_and_block_selections_yank() {
        let mut vim = sample_vim();
        press(&mut vim, "v");
        press(&mut vim, "e");
        assert_eq!(press(&mut vim, "y").yank(), "one");

        let mut vim = sample_vim();
        press(&mut vim, "V");
        press(&mut vim, "j");
        assert_eq!(
            press(&mut vim, "y").yank(),
            "one two-three\n  alpha beta alpha\n"
        );

        let mut vim = VimMode::new(
            vec!["abcd".into(), "efgh".into()],
            Position { row: 0, col: 1 },
            2,
        );
        press(&mut vim, "Ctrl-v");
        press(&mut vim, "l");
        press(&mut vim, "j");
        assert_eq!(press(&mut vim, "y").yank(), "bc\nfg");
    }

    #[test]
    fn yank_with_motion_and_escape_selection_rule() {
        let mut vim = sample_vim();
        press(&mut vim, "y");
        assert_eq!(press(&mut vim, "e").yank(), "one");

        let mut vim = sample_vim();
        press(&mut vim, "y");
        press(&mut vim, "f");
        assert_eq!(press(&mut vim, "-").yank(), "one two-");

        let mut vim = sample_vim();
        press(&mut vim, "G");
        press(&mut vim, "y");
        press(&mut vim, "g");
        assert_eq!(
            press(&mut vim, "g").yank(),
            "one two-three\n  alpha beta alpha\nlast line\n"
        );

        let mut vim = sample_vim();
        press(&mut vim, "2");
        press(&mut vim, "y");
        assert_eq!(
            press(&mut vim, "y").yank(),
            "one two-three\n  alpha beta alpha\n"
        );

        let mut vim = sample_vim();
        press(&mut vim, "w");
        assert_eq!(press(&mut vim, "Y").yank(), "two-three");

        let mut vim = sample_vim();
        press(&mut vim, "w");
        press(&mut vim, "2");
        assert_eq!(press(&mut vim, "Y").yank(), "two-three\n  alpha beta alpha");

        let mut vim = sample_vim();
        press(&mut vim, "v");
        assert!(matches!(press(&mut vim, "Escape"), VimOutcome::None));
        assert!(matches!(press(&mut vim, "Escape"), VimOutcome::Exit));
    }

    #[test]
    fn word_motions_are_safe_on_an_empty_screen() {
        let mut vim = VimMode::new(vec![String::new()], Position { row: 0, col: 0 }, 1);
        press(&mut vim, "w");
        press(&mut vim, "e");
        press(&mut vim, "b");
        assert_eq!(vim.cursor, Position { row: 0, col: 0 });
    }

    impl VimOutcome {
        fn yank(self) -> String {
            match self {
                Self::Yank(text) => text,
                other => panic!("expected yank, got {other:?}"),
            }
        }
    }
}
