#[derive(Clone, Debug)]
pub struct Grid {
    size: Size,
    pos: Pos,
    saved_pos: Pos,
    rows: Vec<crate::row::Row>,
    scroll_top: u16,
    scroll_bottom: u16,
    origin_mode: bool,
    saved_origin_mode: bool,
    scrollback: crate::scrollback::Scrollback,
    scrollback_len: usize,
    scrollback_offset: usize,
    /// Whether the newest scrollback row continues onto the first row of the
    /// screen. Its wrap flag alone cannot say: once that first row has been
    /// cleared, what is printed there is new text, not the rest of the line.
    history_continues: bool,
    /// Whether resizing reflows the contents and trades rows with the
    /// scrollback, as the normal screen does. The alternate screen is only
    /// cropped: the program on it redraws everything at the new size.
    reflow: bool,
    /// What blank cells created by scrolling, inserting and deleting are
    /// filled with: the current background colour only (xterm's BCE).
    fill: crate::attrs::Attrs,
}

impl Grid {
    pub fn new(size: Size, scrollback_len: usize, reflow: bool) -> Self {
        Self {
            size,
            pos: Pos::default(),
            saved_pos: Pos::default(),
            rows: vec![],
            scroll_top: 0,
            scroll_bottom: size.rows - 1,
            origin_mode: false,
            saved_origin_mode: false,
            scrollback: crate::scrollback::Scrollback::default(),
            scrollback_len,
            scrollback_offset: 0,
            history_continues: false,
            reflow,
            fill: crate::attrs::Attrs::default(),
        }
    }

    pub fn set_fill(&mut self, fill: crate::attrs::Attrs) {
        self.fill = fill;
    }

    pub fn allocate_rows(&mut self) {
        if self.rows.is_empty() {
            self.rows = vec![crate::row::Row::new(self.size.cols); usize::from(self.size.rows)];
        }
    }

    fn new_row(&self) -> crate::row::Row {
        let mut row = crate::row::Row::new(self.size.cols);
        if self.fill != crate::attrs::Attrs::default() {
            row.clear(self.fill);
        }
        row
    }

    pub fn clear(&mut self) {
        self.history_continues = false;
        self.saved_pos = Pos::default();
        self.saved_origin_mode = false;
        self.clear_keeping_saved_cursor();
    }

    /// Clears the contents, cursor and margins, but not what DECSC saved:
    /// entering the alternate screen again finds its saved cursor intact.
    pub fn clear_keeping_saved_cursor(&mut self) {
        self.pos = Pos::default();
        for row in &mut self.rows {
            row.clear(crate::attrs::Attrs::default());
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows - 1;
        self.origin_mode = false;
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn set_size(&mut self, size: Size) {
        if size == self.size {
            return;
        }
        if self.reflow && !self.rows.is_empty() {
            self.resize_reflowing(size);
            return;
        }
        // A grid that does not reflow (the alternate screen) never holds
        // history, so it is only cropped. Resizing a row ends its wrap.
        if self.scroll_bottom == self.size.rows - 1 {
            self.scroll_bottom = size.rows - 1;
        }

        self.size = size;
        for row in &mut self.rows {
            row.resize(size.cols, crate::Cell::new());
        }
        self.rows
            .resize(usize::from(size.rows), crate::row::Row::new(size.cols));

        if self.scroll_bottom >= size.rows {
            self.scroll_bottom = size.rows - 1;
        }
        if self.scroll_bottom < self.scroll_top {
            self.scroll_top = 0;
        }

        self.row_clamp_top(false);
        self.row_clamp_bottom(false);
        self.col_clamp();

        if self.saved_pos.row > self.size.rows - 1 {
            self.saved_pos.row = self.size.rows - 1;
        }
        if self.saved_pos.col > self.size.cols - 1 {
            self.saved_pos.col = self.size.cols - 1;
        }
    }

    /// Resizes the normal screen the way tmux does. A new width rewraps the
    /// history and the screen together, and the cursor keeps its place in the
    /// text. A new height then gives up the rows below the cursor before it
    /// sends rows off the top into the history, and a taller screen takes rows
    /// back out of the history, at most as many as it grew by.
    fn resize_reflowing(&mut self, size: Size) {
        let old = self.size;
        let mut cursor_row = usize::from(self.pos.row.min(old.rows - 1));
        // With a wrap pending the cursor is past the last column, after the
        // text rather than on its last character, and stays after it.
        let mut cursor_col = self.pos.col.min(old.cols);
        let mut visible: std::collections::VecDeque<crate::row::Row> =
            std::mem::take(&mut self.rows).into();

        if size.cols != old.cols {
            let mut history = std::mem::take(&mut self.scrollback).into_rows();
            if !self.history_continues {
                if let Some(last) = history.last_mut() {
                    last.wrap(false);
                }
            }
            let history_len = history.len();
            let (mut all, (row, col)) = Self::reflow_rows(
                history.into_iter().chain(visible),
                history_len + cursor_row,
                cursor_col,
                size.cols,
            );
            // The screen keeps its height for now: its last rows, but never
            // starting below the cursor.
            let start = all.len().saturating_sub(usize::from(old.rows)).min(row);
            for _ in 0..start {
                let row = all.pop_front().unwrap();
                self.push_history(row);
            }
            self.saved_pos.row = self
                .saved_pos
                .row
                .saturating_sub(u16::try_from(start).unwrap_or(u16::MAX));
            visible = all;
            cursor_row = row - start;
            cursor_col = col;
        }
        // Only rows of another width are resized: resizing ends a row's wrap.
        for row in &mut visible {
            if row.cols() != size.cols {
                row.resize(size.cols, crate::Cell::new());
            }
        }

        let target = usize::from(size.rows);
        // Rows below the cursor go first, as in tmux: they are blank under a
        // shell, and a full-screen program redraws them anyway.
        while visible.len() > target && visible.len() > cursor_row + 1 {
            visible.pop_back();
        }
        while visible.len() > target && cursor_row > 0 {
            let row = visible.pop_front().unwrap();
            self.push_history(row);
            cursor_row -= 1;
            // The saved cursor stays on the same text.
            self.saved_pos.row = self.saved_pos.row.saturating_sub(1);
        }
        // Text below the cursor that still does not fit is lost, as in tmux.
        visible.truncate(target);
        if size.rows > old.rows {
            let mut growth = usize::from(size.rows - old.rows);
            while growth > 0 && visible.len() < target {
                let Some(mut row) = self.scrollback.pop_back() else {
                    break;
                };
                if row.cols() != size.cols {
                    row.resize(size.cols, crate::Cell::new());
                }
                self.history_continues = self
                    .scrollback
                    .get(self.scrollback.len().wrapping_sub(1))
                    .is_some_and(crate::row::Row::wrapped);
                visible.push_front(row);
                cursor_row += 1;
                self.saved_pos.row = self.saved_pos.row.saturating_add(1);
                growth -= 1;
            }
        }
        while visible.len() < target {
            visible.push_back(crate::row::Row::new(size.cols));
        }
        let mut rows: Vec<crate::row::Row> = visible.into();
        if let Some(last) = rows.last_mut() {
            last.wrap(false);
        }
        for row in &mut rows {
            row.repair_wide();
        }
        self.rows = rows;
        self.size = size;
        self.scroll_top = 0;
        self.scroll_bottom = size.rows - 1;
        let col = cursor_col.min(size.cols);
        self.pos = Pos {
            row: u16::try_from(cursor_row)
                .unwrap_or(u16::MAX)
                .min(size.rows - 1),
            col,
        };
        self.saved_pos.row = self.saved_pos.row.min(size.rows - 1);
        self.saved_pos.col = self.saved_pos.col.min(size.cols - 1);
        self.scrollback_offset = self.scrollback_offset.min(self.scrollback.len());
    }

    fn push_history(&mut self, mut row: crate::row::Row) {
        if self.scrollback_len == 0 {
            return;
        }
        self.history_continues = row.wrapped();
        row.compact();
        if self.scrollback.len() >= self.scrollback_len {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(row);
        if self.scrollback_offset > 0 {
            self.scrollback_offset = self.scrollback.len().min(self.scrollback_offset + 1);
        }
    }

    /// Rewraps `rows` at `cols`, following the cell at row `cursor_row`,
    /// column `cursor_col` to where it lands. Returns the new rows and that
    /// position.
    fn reflow_rows(
        rows: impl Iterator<Item = crate::row::Row>,
        cursor_row: usize,
        cursor_col: u16,
        cols: u16,
    ) -> (std::collections::VecDeque<crate::row::Row>, (usize, u16)) {
        let mut output = std::collections::VecDeque::new();
        let mut logical_line = Vec::new();
        // The cursor's offset into its logical line, once its row is reached.
        let mut cursor_offset: Option<usize> = None;
        let mut cursor = (0, 0);
        for (index, row) in rows.enumerate() {
            let (mut cells, wrapped) = row.into_reflow_cells();
            // A wide character that did not fit at the end of a row left a
            // blank behind before it wrapped; that blank is not text, and
            // rewrapping must not turn it into a space.
            if cells.first().is_some_and(crate::Cell::is_wide)
                && logical_line.last().is_some_and(|cell: &crate::Cell| {
                    !cell.has_contents() && !cell.is_wide_continuation()
                })
            {
                logical_line.pop();
            }
            if index == cursor_row {
                cursor_offset = Some(logical_line.len() + usize::from(cursor_col));
            }
            logical_line.append(&mut cells);
            if !wrapped {
                Self::emit_line(
                    &mut output,
                    &mut logical_line,
                    cols,
                    &mut cursor_offset,
                    &mut cursor,
                );
            }
        }
        if !logical_line.is_empty() || cursor_offset.is_some() {
            Self::emit_line(
                &mut output,
                &mut logical_line,
                cols,
                &mut cursor_offset,
                &mut cursor,
            );
        }
        (output, cursor)
    }

    /// Splits one logical line into rows of `cols`, moving a wide character
    /// that would straddle the edge whole onto the next row, and places the
    /// cursor if it is on this line.
    fn emit_line(
        output: &mut std::collections::VecDeque<crate::row::Row>,
        cells: &mut Vec<crate::Cell>,
        cols: u16,
        cursor_offset: &mut Option<usize>,
        cursor: &mut (usize, u16),
    ) {
        let width = usize::from(cols);
        let first = output.len();
        let mut taken = Vec::new();
        while cells.len() > width {
            let split = if width > 1 && cells[width].is_wide_continuation() {
                width - 1
            } else {
                width
            };
            let remainder = cells.split_off(split);
            let mut row = std::mem::replace(cells, remainder);
            taken.push(split);
            if width > 1 {
                crate::row::repair_wide_cells(&mut row);
            }
            output.push_back(crate::row::Row::from_reflow_cells(row, cols, true));
        }
        taken.push(usize::MAX);
        let mut row = std::mem::take(cells);
        // At one column a wide character's halves land on consecutive rows.
        // They are kept, so widening again joins them back together.
        if width > 1 {
            crate::row::repair_wide_cells(&mut row);
        }
        output.push_back(crate::row::Row::from_reflow_cells(row, cols, false));
        if let Some(mut remaining) = cursor_offset.take() {
            for (index, count) in taken.iter().enumerate() {
                if remaining < *count {
                    // Just past the end of text that fills the last row is a
                    // pending wrap; anywhere further out, the cursor stops at
                    // the last column.
                    let col = if index + 1 == taken.len()
                        && remaining == width
                        && output[first + index]
                            .get(cols - 1)
                            .is_some_and(|cell| cell != crate::Cell::new())
                    {
                        width
                    } else {
                        remaining.min(width - 1)
                    };
                    *cursor = (first + index, u16::try_from(col).unwrap_or(cols - 1));
                    break;
                }
                remaining -= count;
            }
        }
    }

    pub fn pos(&self) -> Pos {
        self.pos
    }

    pub fn set_pos(&mut self, mut pos: Pos) {
        if self.origin_mode {
            pos.row = pos.row.saturating_add(self.scroll_top);
        }
        self.pos = pos;
        self.row_clamp_top(self.origin_mode);
        self.row_clamp_bottom(self.origin_mode);
        self.col_clamp();
    }

    /// Ends a pending wrap: the cursor goes back onto the last column, as
    /// every operation but printing a character does in xterm.
    pub fn clear_pending_wrap(&mut self) {
        if self.pos.col >= self.size.cols {
            self.pos.col = self.size.cols - 1;
        }
    }

    /// DECSTR's share: no origin mode, full-screen margins and a fresh saved
    /// cursor, but the cursor itself stays put.
    pub fn soft_reset(&mut self) {
        self.origin_mode = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows - 1;
        self.saved_pos = Pos::default();
        self.saved_origin_mode = false;
    }

    pub fn save_cursor(&mut self) {
        self.saved_pos = self.pos;
        self.saved_origin_mode = self.origin_mode;
    }

    pub fn restore_cursor(&mut self) {
        self.pos = self.saved_pos;
        self.origin_mode = self.saved_origin_mode;
    }

    /// Packs the scrollback into bytes, in the same form the blocks it is
    /// stored in already use.
    pub fn encode_history(&self) -> Vec<u8> {
        let rows = u32::try_from(self.scrollback.len()).unwrap();
        let mut raw = Vec::new();
        for row in self.scrollback.iter() {
            row.encode(&mut raw);
        }
        let uncompressed_len = raw.len();
        let (body, compressed) = crate::scrollback::compress_if_smaller(raw);
        let mut output = Vec::with_capacity(body.len() + 13);
        output.extend_from_slice(&rows.to_le_bytes());
        output.extend_from_slice(&u64::try_from(uncompressed_len).unwrap().to_le_bytes());
        output.push(u8::from(compressed));
        output.extend_from_slice(&body);
        output
    }

    /// Puts a scrollback packed by [`Self::encode_history`] back, in place of
    /// whatever this grid was holding.
    pub fn restore_history(&mut self, packed: &[u8]) -> bool {
        use std::io::Read as _;

        if packed.len() < 13 {
            return false;
        }
        let rows = u32::from_le_bytes(packed[..4].try_into().unwrap());
        let uncompressed_len = u64::from_le_bytes(packed[4..12].try_into().unwrap());
        if packed[12] > 1 || u64::from(rows) > uncompressed_len / 15 {
            return false;
        }
        // Decode one bounded row at a time. A large valid history must not be
        // rejected merely because its total expanded size exceeds a fixed cap,
        // nor may an untrusted size header cause a matching allocation.
        let body = &packed[13..];
        let mut reader: Box<dyn std::io::Read + '_> = if packed[12] == 1 {
            match zstd::stream::read::Decoder::new(body) {
                Ok(mut decoder) => {
                    // Our encoder uses level 1's small window. Bound malicious
                    // frame windows independently of total decompressed size.
                    if decoder.window_log_max(27).is_err() {
                        return false;
                    }
                    Box::new(decoder)
                }
                Err(_) => return false,
            }
        } else {
            Box::new(body)
        };
        let mut remaining = uncompressed_len;
        let mut restored = crate::scrollback::Scrollback::default();
        for _ in 0..rows {
            let mut header = [0; 15];
            if remaining < 15 || reader.read_exact(&mut header).is_err() {
                return false;
            }
            let cells = usize::from(u16::from_le_bytes(header[7..9].try_into().unwrap()));
            let Ok(data_len) =
                usize::try_from(u32::from_le_bytes(header[9..13].try_into().unwrap()))
            else {
                return false;
            };
            let attrs_len = usize::from(u16::from_le_bytes(header[13..15].try_into().unwrap()));
            // Every cell has at most a two-byte shape entry and 22 text bytes,
            // and at most one 17- or 18-byte attribute span. These format bounds make
            // the allocation independent of a forged total-length declaration.
            if data_len > cells * 24 || attrs_len > cells {
                return false;
            }
            let length = 15 + data_len + attrs_len * crate::row::span_length(header[2]);
            let length64 = u64::try_from(length).unwrap_or(u64::MAX);
            if length64 > remaining {
                return false;
            }
            let mut record = Vec::with_capacity(length);
            record.extend_from_slice(&header);
            record.resize(length, 0);
            if reader.read_exact(&mut record[15..]).is_err() {
                return false;
            }
            let Some(row) = crate::row::Row::decode_checked(&mut record.as_slice()) else {
                return false;
            };
            remaining -= length64;
            if self.scrollback_len > 0 {
                if restored.len() == self.scrollback_len {
                    restored.pop_front();
                }
                restored.push_back(row);
            }
        }
        // Require exact framing, including decoder errors at the frame tail.
        if remaining != 0 || !matches!(reader.read(&mut [0]), Ok(0)) {
            return false;
        }
        self.scrollback = restored;
        self.scrollback_offset = self.scrollback_offset.min(self.scrollback.len());
        true
    }

    pub fn all_rows(&self) -> impl Iterator<Item = &crate::row::Row> {
        self.scrollback.iter().chain(self.rows.iter())
    }

    pub fn visible_rows(&self) -> impl Iterator<Item = &crate::row::Row> {
        let scrollback_len = self.scrollback.len();
        let rows_len = self.rows.len();
        self.scrollback
            .iter()
            .skip(scrollback_len - self.scrollback_offset)
            // Scrolled back further than a screenful, the screen shows
            // nothing but history.
            .take(rows_len)
            .chain(
                self.rows
                    .iter()
                    .take(rows_len.saturating_sub(self.scrollback_offset)),
            )
    }

    pub fn visible_row(&self, row: u16) -> Option<&crate::row::Row> {
        let row = usize::from(row);
        if row >= self.rows.len() {
            return None;
        }
        let history_rows = self.scrollback_offset.min(self.rows.len());
        if row < history_rows {
            self.scrollback
                .get(self.scrollback.len() - self.scrollback_offset + row)
        } else {
            self.rows.get(row - history_rows)
        }
    }

    pub fn drawing_row(&self, row: u16) -> Option<&crate::row::Row> {
        self.rows.get(usize::from(row))
    }

    pub fn drawing_row_mut(&mut self, row: u16) -> Option<&mut crate::row::Row> {
        self.rows.get_mut(usize::from(row))
    }

    fn current_row_mut(&mut self) -> &mut crate::row::Row {
        &mut self.rows[usize::from(self.pos.row)]
    }

    pub fn visible_cell(&self, pos: Pos) -> Option<crate::Cell> {
        self.visible_row(pos.row).and_then(|r| r.get(pos.col))
    }

    pub fn drawing_cell(&self, pos: Pos) -> Option<crate::Cell> {
        self.drawing_row(pos.row).and_then(|r| r.get(pos.col))
    }

    pub fn drawing_cell_mut(&mut self, pos: Pos) -> Option<&mut crate::Cell> {
        self.drawing_row_mut(pos.row)
            .and_then(|r| r.get_mut(pos.col))
    }

    /// How many rows of history the grid is holding right now, as opposed to
    /// the most it would hold.
    pub fn history_rows(&self) -> usize {
        self.scrollback.len()
    }

    pub fn scrollback(&self) -> usize {
        self.scrollback_offset
    }

    pub fn history_bytes(&self) -> usize {
        self.scrollback.heap_bytes()
    }

    pub fn set_history_backing(&mut self, file: std::fs::File) {
        self.scrollback.set_backing(file);
    }

    pub fn flush_history_backing(&self) {
        self.scrollback.flush_backing();
    }

    pub fn set_scrollback(&mut self, rows: usize) {
        self.scrollback_offset = rows.min(self.scrollback.len());
        if self.scrollback_offset == 0 {
            self.scrollback.clear_decoded();
        }
    }

    pub fn write_contents(&self, contents: &mut String) {
        let mut wrapping = false;
        for row in self.visible_rows() {
            row.write_contents(contents, 0, self.size.cols, wrapping);
            if !row.wrapped() {
                contents.push('\n');
            }
            wrapping = row.wrapped();
        }

        while contents.ends_with('\n') {
            contents.truncate(contents.len() - 1);
        }
    }

    pub fn write_contents_formatted(&self, contents: &mut Vec<u8>) -> crate::attrs::Attrs {
        crate::term::clear_attrs(contents);
        crate::term::clear_screen(contents);

        let mut prev_attrs = crate::attrs::Attrs::default();
        let mut prev_pos = Pos::default();
        let mut wrapping = false;
        for (row, i) in self.visible_rows().zip(0..) {
            (prev_pos, prev_attrs) = row.write_contents_formatted(
                contents,
                self.size.cols,
                i,
                wrapping,
                prev_pos,
                prev_attrs,
            );
            wrapping = row.wrapped();
        }

        self.write_cursor_position_formatted(contents, prev_pos, prev_attrs);
        prev_attrs
    }

    pub fn write_contents_diff(
        &self,
        contents: &mut Vec<u8>,
        prev: &Self,
        mut prev_attrs: crate::attrs::Attrs,
    ) -> crate::attrs::Attrs {
        let mut prev_pos = prev.pos;
        let mut wrapping = false;
        let mut prev_wrapping = false;
        for ((row, prev_row), i) in self.visible_rows().zip(prev.visible_rows()).zip(0..) {
            (prev_pos, prev_attrs) = row.write_contents_diff(
                contents,
                prev_row,
                self.size.cols,
                i,
                wrapping,
                prev_wrapping,
                prev_pos,
                prev_attrs,
            );
            wrapping = row.wrapped();
            prev_wrapping = prev_row.wrapped();
        }

        self.write_cursor_position_formatted(contents, prev_pos, prev_attrs);
        prev_attrs
    }

    /// Where the last character of `row` starts: the last column, or the
    /// one before it when that holds the second half of a wide character.
    fn last_char_pos(&self, row: u16) -> Pos {
        let mut pos = Pos {
            row,
            col: self.size.cols - 1,
        };
        if self.size.cols >= 2 && self.drawing_cell(pos).unwrap().is_wide_continuation() {
            pos.col -= 1;
        }
        pos
    }

    fn write_cursor_position_formatted(
        &self,
        contents: &mut Vec<u8>,
        prev_pos: Pos,
        prev_attrs: crate::attrs::Attrs,
    ) {
        if prev_pos == self.pos || self.pos.col < self.size.cols {
            crate::term::move_from_to(contents, prev_pos, self.pos);
            return;
        }
        // writing a character to the last column of a row doesn't wrap the
        // cursor immediately - it waits until the next character is actually
        // drawn. it is only possible for the cursor to have this kind of
        // position after drawing a character though, so if we end in this
        // position, we need to redraw the character at the end of the row.
        let redraw = |contents: &mut Vec<u8>, pos, cell: &crate::Cell| {
            crate::term::move_from_to(contents, prev_pos, pos);
            cell.attrs().write_escape_code_diff(contents, &prev_attrs);
            contents.extend(cell.contents().as_bytes());
            prev_attrs.write_escape_code_diff(contents, cell.attrs());
        };
        let pos = self.last_char_pos(self.pos.row);
        let cell = self.drawing_cell(pos).unwrap();
        if cell.has_contents() {
            redraw(contents, pos, &cell);
            return;
        }

        // Otherwise we got here from a newline while already after the end
        // of an earlier row: get after that row and write the newlines.
        for row in (0..self.pos.row).rev() {
            let pos = self.last_char_pos(row);
            let cell = self.drawing_cell(pos).unwrap();
            if cell.has_contents() {
                if prev_pos.row != row || prev_pos.col < self.size.cols {
                    redraw(contents, pos, &cell);
                }
                contents.extend("\n".repeat(usize::from(self.pos.row - row)).as_bytes());
                return;
            }
        }

        // The end of the row was cleared (IL, DL, ED, EL, ...) without
        // moving the cursor. Nothing is in the last column, so it is safe to
        // draw there and erase it again, keeping its attributes.
        let pos = Pos {
            row: self.pos.row,
            col: self.size.cols - 1,
        };
        crate::term::move_from_to(contents, prev_pos, pos);
        contents.push(b' ');
        let end_cell = self.drawing_cell(pos).unwrap();
        end_cell
            .attrs()
            .write_escape_code_diff(contents, &prev_attrs);
        crate::term::save_cursor(contents);
        crate::term::backspace(contents);
        crate::term::erase_char(contents, 1);
        crate::term::restore_cursor(contents);
        prev_attrs.write_escape_code_diff(contents, end_cell.attrs());
    }

    /// ED 3: forgets the scrollback.
    pub fn erase_history(&mut self) {
        self.scrollback.clear();
        self.scrollback_offset = 0;
        self.history_continues = false;
    }

    /// Records that the first row of the screen was cleared or replaced, so
    /// nothing printed there continues the newest scrollback row.
    fn first_row_replaced(&mut self) {
        self.history_continues = false;
    }

    pub fn erase_all(&mut self, attrs: crate::attrs::Attrs) {
        self.first_row_replaced();
        for row in &mut self.rows {
            row.clear(attrs);
        }
    }

    pub fn erase_all_forward(&mut self, attrs: crate::attrs::Attrs) {
        let row = usize::from(self.pos.row);
        if row == 0 && self.pos.col == 0 {
            self.first_row_replaced();
        }
        for row in &mut self.rows[row + 1..] {
            row.clear(attrs);
        }
        self.erase_row_forward(attrs);
    }

    pub fn erase_all_backward(&mut self, attrs: crate::attrs::Attrs) {
        self.first_row_replaced();
        for row in &mut self.rows[..usize::from(self.pos.row)] {
            row.clear(attrs);
        }
        self.erase_row_backward(attrs);
    }

    pub fn erase_row(&mut self, attrs: crate::attrs::Attrs) {
        if self.pos.row == 0 {
            self.first_row_replaced();
        }
        self.current_row_mut().clear(attrs);
    }

    pub fn erase_row_forward(&mut self, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in pos.col..size.cols {
            row.erase(col, attrs);
        }
    }

    pub fn erase_row_backward(&mut self, attrs: crate::attrs::Attrs) {
        if self.pos.row == 0 {
            self.first_row_replaced();
        }
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in 0..=pos.col.min(size.cols - 1) {
            row.erase(col, attrs);
        }
    }

    /// ICH. Only the cells from the cursor to the edge can move, so no more
    /// than that many are inserted: a huge count would otherwise cost time
    /// quadratic in it, freezing the whole daemon. A wide character the
    /// insertion splits, or pushes half off the edge, is blanked.
    pub fn insert_cells(&mut self, count: u16) {
        let size = self.size;
        let pos = self.pos;
        if pos.col >= size.cols {
            return;
        }
        let count = count.min(size.cols - pos.col);
        let mut blank = crate::Cell::new();
        blank.clear(self.fill);
        let row = self.current_row_mut();
        for _ in 0..count {
            row.insert(pos.col, blank.clone());
        }
        row.truncate(size.cols);
        row.repair_wide();
    }

    pub fn delete_cells(&mut self, count: u16) {
        let size = self.size;
        let pos = self.pos;
        let mut blank = crate::Cell::new();
        blank.clear(self.fill);
        let row = self.current_row_mut();
        for _ in 0..(count.min(size.cols.saturating_sub(pos.col))) {
            row.remove(pos.col);
        }
        row.resize(size.cols, blank);
        row.repair_wide();
    }

    pub fn erase_cells(&mut self, count: u16, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in pos.col..((pos.col.saturating_add(count)).min(size.cols)) {
            row.erase(col, attrs);
        }
    }

    /// IL. Outside the scroll region it does nothing, as in xterm; inside,
    /// only the rows from the cursor to the bottom margin move.
    pub fn insert_lines(&mut self, count: u16) {
        if !self.in_scroll_region() {
            return;
        }
        if self.pos.row == 0 {
            self.first_row_replaced();
        }
        let count = count.min(self.scroll_bottom - self.pos.row + 1);
        for _ in 0..count {
            self.rows.remove(usize::from(self.scroll_bottom));
            self.rows.insert(usize::from(self.pos.row), self.new_row());
        }
        // self.scroll_bottom is maintained to always be a valid row
        self.rows[usize::from(self.scroll_bottom)].wrap(false);
        self.pos.col = 0;
    }

    /// DL, bounded by the scroll region like IL.
    pub fn delete_lines(&mut self, count: u16) {
        if !self.in_scroll_region() {
            return;
        }
        if self.pos.row == 0 {
            self.first_row_replaced();
        }
        let count = count.min(self.scroll_bottom - self.pos.row + 1);
        for _ in 0..count {
            self.rows
                .insert(usize::from(self.scroll_bottom) + 1, self.new_row());
            self.rows.remove(usize::from(self.pos.row));
        }
        self.pos.col = 0;
    }

    pub fn scroll_up(&mut self, count: u16) {
        for _ in 0..(count.min(self.scroll_bottom - self.scroll_top + 1)) {
            self.rows
                .insert(usize::from(self.scroll_bottom) + 1, self.new_row());
            let removed = self.rows.remove(usize::from(self.scroll_top));
            if self.scrollback_len > 0 && self.scroll_top == 0 {
                self.push_history(removed);
            }
        }
    }

    pub fn scroll_down(&mut self, count: u16) {
        if self.scroll_top == 0 && count > 0 {
            self.first_row_replaced();
        }
        for _ in 0..(count.min(self.scroll_bottom - self.scroll_top + 1)) {
            self.rows.remove(usize::from(self.scroll_bottom));
            self.rows
                .insert(usize::from(self.scroll_top), self.new_row());
            // self.scroll_bottom is maintained to always be a valid row
            self.rows[usize::from(self.scroll_bottom)].wrap(false);
        }
    }

    /// DECSTBM. A region of fewer than two rows is ignored, as in xterm.
    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let bottom = bottom.min(self.size().rows - 1);
        if top >= bottom {
            return;
        }
        self.scroll_top = top;
        self.scroll_bottom = bottom;
        self.set_pos(Pos { row: 0, col: 0 });
    }

    /// The cursor, margins and origin mode, which xterm keeps once for the
    /// terminal rather than per screen buffer.
    pub fn shared_state(&self) -> (Pos, u16, u16, bool) {
        (
            self.pos,
            self.scroll_top,
            self.scroll_bottom,
            self.origin_mode,
        )
    }

    pub fn set_shared_state(&mut self, (pos, top, bottom, origin): (Pos, u16, u16, bool)) {
        self.pos = pos;
        self.scroll_top = top;
        self.scroll_bottom = bottom;
        self.origin_mode = origin;
    }

    fn in_scroll_region(&self) -> bool {
        self.pos.row >= self.scroll_top && self.pos.row <= self.scroll_bottom
    }

    pub fn set_origin_mode(&mut self, mode: bool) {
        self.origin_mode = mode;
        self.set_pos(Pos { row: 0, col: 0 });
    }

    pub fn row_inc_clamp(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_add(count);
        self.row_clamp_bottom(in_scroll_region);
    }

    pub fn row_inc_scroll(&mut self, count: u16) -> u16 {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_add(count);
        let lines = self.row_clamp_bottom(in_scroll_region);
        if in_scroll_region {
            self.scroll_up(lines);
            lines
        } else {
            0
        }
    }

    pub fn row_dec_clamp(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_sub(count);
        self.row_clamp_top(in_scroll_region);
    }

    pub fn row_dec_scroll(&mut self, count: u16) {
        if !self.in_scroll_region() {
            // Outside the region nothing scrolls; the cursor only moves, and
            // stops at the top of the screen.
            self.pos.row = self.pos.row.saturating_sub(count);
            return;
        }
        let lines = count.saturating_sub(self.pos.row - self.scroll_top);
        self.pos.row = self.pos.row.saturating_sub(count).max(self.scroll_top);
        self.scroll_down(lines);
    }

    /// VPA: in origin mode the row counts from the top margin and stays
    /// inside the region, like CUP.
    pub fn row_set(&mut self, i: u16) {
        self.pos.row = if self.origin_mode {
            i.saturating_add(self.scroll_top)
        } else {
            i
        };
        self.row_clamp_top(self.origin_mode);
        self.row_clamp_bottom(self.origin_mode);
    }

    pub fn col_inc(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_add(count);
    }

    pub fn col_inc_clamp(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_add(count);
        self.col_clamp();
    }

    pub fn col_dec(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_sub(count);
    }

    pub fn col_set(&mut self, i: u16) {
        self.pos.col = i;
        self.col_clamp();
    }

    pub fn col_wrap(&mut self, width: u16, wrap: bool) {
        // A grid narrower than the character being drawn has no column the
        // character fits in, so there is no position to wrap out of.
        if self.pos.col > self.size.cols.saturating_sub(width) {
            let mut prev_pos = self.pos;
            self.pos.col = 0;
            let scrolled = self.row_inc_scroll(1);
            let new_pos = self.pos;
            // Wrapping out of the last row of the scroll region scrolls the
            // row that was just filled. When that region is a single row at
            // the top of the grid, the row goes straight to scrollback and
            // there is nothing left on screen to mark as wrapped.
            let Some(row) = prev_pos.row.checked_sub(scrolled) else {
                return;
            };
            prev_pos.row = row;
            let wrapped = wrap && prev_pos.row + 1 == new_pos.row;
            if let Some(row) = self.drawing_row_mut(prev_pos.row) {
                row.wrap(wrapped);
            }
        }
    }

    fn row_clamp_top(&mut self, limit_to_scroll_region: bool) -> u16 {
        if limit_to_scroll_region && self.pos.row < self.scroll_top {
            let rows = self.scroll_top - self.pos.row;
            self.pos.row = self.scroll_top;
            rows
        } else {
            0
        }
    }

    fn row_clamp_bottom(&mut self, limit_to_scroll_region: bool) -> u16 {
        let bottom = if limit_to_scroll_region {
            self.scroll_bottom
        } else {
            self.size.rows - 1
        };
        if self.pos.row > bottom {
            let rows = self.pos.row - bottom;
            self.pos.row = bottom;
            rows
        } else {
            0
        }
    }

    fn col_clamp(&mut self) {
        if self.pos.col > self.size.cols - 1 {
            self.pos.col = self.size.cols - 1;
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
}

#[cfg(test)]
mod history_restore_tests {
    use super::{Grid, Pos, Size};

    fn labeled_grid(limit: usize) -> Grid {
        let mut grid = Grid::new(Size { rows: 6, cols: 8 }, limit, limit > 0);
        grid.allocate_rows();
        for (index, row) in grid.rows.iter_mut().enumerate() {
            row.get_mut(0).unwrap().set(
                char::from(b'A' + u8::try_from(index).unwrap()),
                crate::attrs::Attrs::default(),
            );
        }
        grid
    }

    fn labels(grid: &Grid) -> Vec<String> {
        grid.all_rows()
            .map(|row| row.get(0).unwrap().contents().to_string())
            .collect()
    }

    #[test]
    fn shrink_only_moves_rows_when_the_live_cursor_overflows() {
        let mut grid = labeled_grid(100);
        grid.pos = Pos { row: 1, col: 2 };
        grid.saved_pos = Pos { row: 5, col: 7 };
        grid.set_size(Size { rows: 2, cols: 8 });
        assert_eq!(labels(&grid), ["A", "B"]);
        assert_eq!(grid.pos, Pos { row: 1, col: 2 });
        assert_eq!(grid.saved_pos, Pos { row: 1, col: 7 });
        assert_eq!(grid.scrollback.len(), 0);
    }

    #[test]
    fn shrink_projects_saved_cursor_margins_and_history_offset() {
        let mut grid = labeled_grid(100);
        grid.pos = Pos { row: 5, col: 7 };
        grid.saved_pos = Pos { row: 4, col: 3 };
        grid.scroll_top = 2;
        grid.scroll_bottom = 4;
        grid.set_size(Size { rows: 2, cols: 8 });
        assert_eq!(labels(&grid), ["A", "B", "C", "D", "E", "F"]);
        assert_eq!(grid.pos, Pos { row: 1, col: 7 });
        assert_eq!(grid.saved_pos, Pos { row: 0, col: 3 });
        // The margins reset to the whole screen, as in tmux.
        assert_eq!((grid.scroll_top, grid.scroll_bottom), (0, 1));
        grid.set_scrollback(2);
        grid.set_size(Size { rows: 1, cols: 8 });
        assert_eq!(labels(&grid), ["A", "B", "C", "D", "E", "F"]);
        assert_eq!(grid.scrollback(), 3);
        assert_eq!(grid.visible_row(0).unwrap().get(0).unwrap().contents(), "C");
        assert_eq!(grid.pos, Pos { row: 0, col: 7 });
    }

    #[test]
    fn shrink_honors_history_capacity_and_disabled_history() {
        let mut bounded = labeled_grid(2);
        bounded.pos = Pos { row: 5, col: 0 };
        bounded.set_size(Size { rows: 1, cols: 8 });
        assert_eq!(labels(&bounded), ["D", "E", "F"]);
        assert_eq!(bounded.scrollback.len(), 2);
        let mut disabled = labeled_grid(0);
        disabled.pos = Pos { row: 5, col: 0 };
        disabled.set_size(Size { rows: 1, cols: 8 });
        assert_eq!(labels(&disabled), ["A"]);
        assert_eq!(disabled.scrollback.len(), 0);
        assert_eq!(disabled.pos, Pos { row: 0, col: 0 });
    }

    #[test]
    fn simultaneous_shrink_reflows_promoted_history_before_clipping_columns() {
        let mut parser = crate::Parser::new(3, 8, 100);
        parser.process(b"\x1b[1;2;4:3;38;2;1;2;3mABCDEFG\r\nHIJKLMN\r\nO");
        parser.screen_mut().set_size(1, 4);
        let rows: Vec<_> = parser.screen().all_rows().collect();
        let text: Vec<_> = rows
            .iter()
            .map(|row| {
                (0..4)
                    .map(|x| row.get(x).unwrap().contents().to_string())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(text, ["ABCD", "EFG", "HIJK", "LMN", "O"]);
        for row in rows {
            let first = row.get(0).unwrap();
            assert!(first.bold() && first.dim());
            assert_eq!(first.underline_style(), crate::UnderlineStyle::Curly);
            assert_eq!(first.fgcolor(), crate::Color::Rgb(1, 2, 3));
        }
        parser.process(b"\x1b[?1049hALT");
        parser.screen_mut().set_size(1, 2);
        assert_eq!(parser.screen().all_rows().count(), 1);
        parser.process(b"\x1b[?1049l");
        assert!(parser.screen().all_rows().count() > 1);
    }

    #[test]
    fn alternate_screen_shrink_clips_without_polluting_primary_history() {
        let mut parser = crate::Parser::new(6, 8, 100);
        parser.process(b"A\r\nB\r\nC\r\nD\r\nE\r\nF\x1b[?1049ha\r\nb\r\nc\r\nd\r\ne\r\nf");
        parser.screen_mut().set_size(2, 8);
        // ?1049h leaves the cursor where it was, as in xterm, so the
        // alternate screen's text starts after the F.
        assert_eq!(parser.screen().contents(), " a\nb");
        assert_eq!(parser.screen().cursor_position(), (1, 1));
        assert_eq!(parser.screen().all_rows().count(), 2);
        parser.process(b"\x1b[?1049l");
        let first_cells: Vec<_> = parser
            .screen()
            .all_rows()
            .map(|row| row.get(0).unwrap().contents().to_string())
            .collect();
        assert_eq!(first_cells, ["A", "B", "C", "D", "E", "F"]);
    }

    #[test]
    fn large_styled_history_streams_and_retains_only_the_configured_tail() {
        // A supported wide terminal with a different RGB style on every cell
        // produces a large expanded history even though zstd packs it tightly.
        let mut terminal = crate::Parser::new(2, 4096, 0);
        let mut input = Vec::new();
        for col in 0..4096 {
            input.extend_from_slice(
                format!("\x1b[3;4:5;38;2;{};2;3;58;2;4;5;6mA", col % 2).as_bytes(),
            );
        }
        terminal.process(&input);
        let template = terminal.screen().all_rows().next().unwrap().clone();
        let mut source = Grid::new(
            Size {
                rows: 2,
                cols: 4096,
            },
            960,
            true,
        );
        for index in 0..960 {
            let mut row = template.clone();
            let character = match index {
                957 => 'X',
                958 => 'Y',
                959 => 'Z',
                _ => 'A',
            };
            row.get_mut(0)
                .unwrap()
                .set(character, crate::attrs::Attrs::default());
            source.scrollback.push_back(row);
        }
        let packed = source.encode_history();
        let expanded = u64::from_le_bytes(packed[4..12].try_into().unwrap());
        assert!(expanded > 64 * 1024 * 1024);
        assert_eq!(packed[12], 1);
        assert!(
            packed.len() < 16 * 1024 * 1024,
            "fits a pane journal record"
        );
        let mut restored = Grid::new(
            Size {
                rows: 2,
                cols: 4096,
            },
            3,
            true,
        );
        assert!(restored.restore_history(&packed));
        let check_tail = |grid: &Grid| {
            let rows: Vec<_> = grid.scrollback.iter().collect();
            assert_eq!(rows.len(), 3);
            for (row, expected) in rows.iter().zip(["X", "Y", "Z"]) {
                assert_eq!(row.get(0).unwrap().contents(), expected);
                let styled = row.get(1).unwrap();
                assert!(styled.italic());
                assert_eq!(styled.underline_style(), crate::UnderlineStyle::Dashed);
                assert_eq!(styled.fgcolor(), crate::Color::Rgb(1, 2, 3));
                assert_eq!(styled.underline_color(), crate::Color::Rgb(4, 5, 6));
            }
        };
        check_tail(&restored);
        // A late truncated frame must not install the successfully decoded prefix.
        assert!(!restored.restore_history(&packed[..packed.len() - 1]));
        check_tail(&restored);
        let mut forged = packed.clone();
        forged[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(!restored.restore_history(&forged));
        check_tail(&restored);

        let raw =
            zstd::bulk::decompress(&packed[13..], usize::try_from(expanded).unwrap()).unwrap();
        let mut uncompressed = packed[..13].to_vec();
        uncompressed[12] = 0;
        uncompressed.extend_from_slice(&raw);
        assert!(restored.restore_history(&uncompressed));
        check_tail(&restored);
    }
}
