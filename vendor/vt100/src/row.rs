use crate::term::BufWrite as _;

#[derive(Clone, Debug)]
/// One row of a terminal: its cells, how wide it is, and whether the line it
/// belongs to carries on into the next row.
pub struct Row {
    cells: RowCells,
    cols: u16,
    wrapped: bool,
}

#[derive(Clone, Debug)]
enum RowCells {
    Active(Vec<crate::Cell>),
    Compact(CompactCells),
}

#[derive(Clone, Debug)]
struct CompactCells {
    data: Box<[u8]>,
    shape_len: u32,
    cells: u16,
    // The extra indirection makes the common `None` representation one word
    // instead of the two words required by `Box<[AttrSpan]>`.
    #[allow(clippy::box_collection)]
    attrs: Option<Box<Vec<AttrSpan>>>,
}

#[derive(Clone, Copy, Debug)]
struct AttrSpan {
    start: u16,
    end: u16,
    attrs: crate::attrs::Attrs,
}

/// The cells of a [`Row`], unpacked as they are walked.
pub struct Cells<'a> {
    row: &'a Row,
    col: u16,
    shape_index: usize,
    run_remaining: u16,
    run_len: u8,
    content_start: usize,
    attr_index: usize,
}

impl CompactCells {
    fn cell(&self, col: u16) -> crate::Cell {
        if col >= self.cells {
            return crate::Cell::new();
        }
        let shape_len = usize::try_from(self.shape_len).unwrap();
        let mut cell = 0u16;
        let mut start = 0usize;
        let mut len = 0u8;
        for run in self.data[..shape_len].chunks_exact(2) {
            let count = u16::from(run[0]);
            let run_len = run[1];
            if col < cell + count {
                start += usize::from(col - cell) * usize::from(run_len & crate::cell::LEN_BITS);
                len = run_len;
                break;
            }
            start += usize::from(count) * usize::from(run_len & crate::cell::LEN_BITS);
            cell += count;
        }
        let end = start + usize::from(len & crate::cell::LEN_BITS);
        let attrs = self
            .attrs
            .iter()
            .flat_map(|attrs| attrs.iter())
            .find(|span| col >= span.start && col < span.end)
            .map_or_else(crate::attrs::Attrs::default, |span| span.attrs);
        crate::Cell::from_compact(&self.data[shape_len + start..shape_len + end], len, attrs)
    }
}

impl Row {
    pub(crate) fn new(cols: u16) -> Self {
        Self {
            cells: RowCells::Active(vec![crate::Cell::new(); usize::from(cols)]),
            cols,
            wrapped: false,
        }
    }

    fn cols(&self) -> u16 {
        self.cols
    }

    /// How many cells from the left differ from a blank cell.
    ///
    /// A compacted row already knows: everything past it was dropped when the
    /// row was compacted. Reading a long scrollback means decoding every cell
    /// of every row, so skipping the blank tail is most of that work.
    pub fn used_cells(&self) -> u16 {
        match &self.cells {
            RowCells::Active(_) => self.cols,
            RowCells::Compact(compact) => compact.cells,
        }
    }

    pub(crate) fn clear(&mut self, attrs: crate::attrs::Attrs) {
        for cell in self.active_cells() {
            cell.clear(attrs);
        }
        self.wrapped = false;
    }

    /// Every cell in the row, unpacked as it is reached.
    pub fn cells(&self) -> Cells<'_> {
        Cells {
            row: self,
            col: 0,
            shape_index: 0,
            run_remaining: 0,
            run_len: 0,
            content_start: 0,
            attr_index: 0,
        }
    }

    pub(crate) fn get(&self, col: u16) -> Option<crate::Cell> {
        if col >= self.cols {
            return None;
        }
        match &self.cells {
            RowCells::Active(cells) => Some(cells[usize::from(col)].clone()),
            RowCells::Compact(cells) => Some(cells.cell(col)),
        }
    }

    pub(crate) fn get_mut(&mut self, col: u16) -> Option<&mut crate::Cell> {
        self.active_cells().get_mut(usize::from(col))
    }

    pub(crate) fn insert(&mut self, i: u16, cell: crate::Cell) {
        self.active_cells().insert(usize::from(i), cell);
        self.wrapped = false;
    }

    pub(crate) fn remove(&mut self, i: u16) {
        self.clear_wide(i);
        self.active_cells().remove(usize::from(i));
        self.wrapped = false;
    }

    pub(crate) fn erase(&mut self, i: u16, attrs: crate::attrs::Attrs) {
        let wide = self.get(i).unwrap().is_wide();
        self.clear_wide(i);
        self.active_cells()[usize::from(i)].clear(attrs);
        if i == self.cols() - if wide { 2 } else { 1 } {
            self.wrapped = false;
        }
    }

    pub(crate) fn truncate(&mut self, len: u16) {
        self.active_cells().truncate(usize::from(len));
        self.cols = len;
        self.wrapped = false;
        let last_cell = &mut self.active_cells()[usize::from(len) - 1];
        if last_cell.is_wide() {
            last_cell.clear(*last_cell.attrs());
        }
    }

    pub(crate) fn resize(&mut self, len: u16, cell: crate::Cell) {
        self.active_cells().resize(usize::from(len), cell);
        self.cols = len;
        self.wrapped = false;
    }

    pub(crate) fn wrap(&mut self, wrap: bool) {
        self.wrapped = wrap;
    }

    pub(crate) fn wrapped(&self) -> bool {
        self.wrapped
    }

    pub(crate) fn into_reflow_cells(self) -> (Vec<crate::Cell>, bool) {
        let wrapped = self.wrapped;
        let mut cells: Vec<_> = self.cells().collect();
        if !wrapped {
            let default = crate::Cell::new();
            let len = cells
                .iter()
                .rposition(|cell| cell != &default)
                .map_or(0, |index| index + 1);
            cells.truncate(len);
        }
        (cells, wrapped)
    }

    pub(crate) fn from_reflow_cells(mut cells: Vec<crate::Cell>, cols: u16, wrapped: bool) -> Self {
        cells.resize(usize::from(cols), crate::Cell::new());
        let mut row = Self {
            cells: RowCells::Active(cells),
            cols,
            wrapped,
        };
        row.compact();
        row
    }

    pub(crate) fn clear_wide(&mut self, col: u16) {
        let cell = self.get(col).unwrap();
        let other_col = if cell.is_wide() {
            col + 1
        } else if cell.is_wide_continuation() {
            col - 1
        } else {
            return;
        };
        let other = &mut self.active_cells()[usize::from(other_col)];
        other.clear(*other.attrs());
    }

    pub(crate) fn compact(&mut self) {
        if matches!(self.cells, RowCells::Compact(_)) {
            return;
        }
        let default = crate::Cell::new();
        let used = (0..self.cols)
            .rev()
            .find(|col| self.get(*col).is_some_and(|cell| cell != default))
            .map_or(0, |col| usize::from(col) + 1);
        let cells: Vec<_> = self.cells().take(used).collect();
        let mut contents = Vec::new();
        let mut shape = Vec::new();
        let mut attrs = Vec::new();
        for (index, cell) in cells.iter().enumerate() {
            let index: u16 = index.try_into().unwrap();
            contents.extend_from_slice(cell.contents().as_bytes());
            let len = cell.compact_len();
            if shape.len() >= 2 && shape[shape.len() - 1] == len && shape[shape.len() - 2] < u8::MAX
            {
                let count = shape.len() - 2;
                shape[count] += 1;
            } else {
                shape.extend_from_slice(&[1, len]);
            }
            if *cell.attrs() == crate::attrs::Attrs::default() {
                continue;
            }
            if attrs
                .last()
                .is_none_or(|span: &AttrSpan| span.end != index || span.attrs != *cell.attrs())
            {
                attrs.push(AttrSpan {
                    start: index,
                    end: index + 1,
                    attrs: *cell.attrs(),
                });
            } else {
                attrs.last_mut().unwrap().end = index + 1;
            }
        }
        attrs.shrink_to_fit();
        let shape_len = shape.len().try_into().unwrap();
        shape.extend_from_slice(&contents);
        self.cells = RowCells::Compact(CompactCells {
            data: shape.into_boxed_slice(),
            shape_len,
            cells: used.try_into().unwrap(),
            attrs: (!attrs.is_empty()).then(|| Box::new(attrs)),
        });
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        match &self.cells {
            RowCells::Active(cells) => cells.capacity() * std::mem::size_of::<crate::Cell>(),
            RowCells::Compact(cells) => {
                cells.data.len()
                    + cells.attrs.as_ref().map_or(0, |attrs| {
                        std::mem::size_of::<Vec<AttrSpan>>()
                            + attrs.capacity() * std::mem::size_of::<AttrSpan>()
                    })
            }
        }
    }

    pub(crate) fn encode(&self, output: &mut Vec<u8>) {
        let RowCells::Compact(cells) = &self.cells else {
            unreachable!("only compact rows enter scrollback blocks")
        };
        output.extend_from_slice(&self.cols.to_le_bytes());
        output.push(u8::from(self.wrapped));
        output.extend_from_slice(&cells.shape_len.to_le_bytes());
        output.extend_from_slice(&cells.cells.to_le_bytes());
        output.extend_from_slice(
            &u32::try_from(cells.data.len()).unwrap().to_le_bytes(),
        );
        output.extend_from_slice(
            &u16::try_from(cells.attrs.as_ref().map_or(0, |attrs| attrs.len()))
                .unwrap()
                .to_le_bytes(),
        );
        output.extend_from_slice(&cells.data);
        for span in cells.attrs.iter().flat_map(|attrs| attrs.iter()) {
            output.extend_from_slice(&span.start.to_le_bytes());
            output.extend_from_slice(&span.end.to_le_bytes());
            encode_attrs(output, span.attrs);
        }
    }

    pub(crate) fn decode(input: &mut &[u8]) -> Self {
        let cols = read_u16(input);
        let wrapped = take(input, 1)[0] != 0;
        let shape_len = read_u32(input);
        let cells = read_u16(input);
        let data_len = usize::try_from(read_u32(input)).unwrap();
        let attrs_len = usize::from(read_u16(input));
        let data = take(input, data_len).to_vec().into_boxed_slice();
        let attrs = (attrs_len > 0).then(|| {
            let mut attrs = Vec::with_capacity(attrs_len);
            for _ in 0..attrs_len {
                attrs.push(AttrSpan {
                    start: read_u16(input),
                    end: read_u16(input),
                    attrs: decode_attrs(input),
                });
            }
            Box::new(attrs)
        });
        Self {
            cells: RowCells::Compact(CompactCells {
                data,
                shape_len,
                cells,
                attrs,
            }),
            cols,
            wrapped,
        }
    }

    fn active_cells(&mut self) -> &mut Vec<crate::Cell> {
        if let RowCells::Compact(_) = self.cells {
            let cells = self.cells().collect();
            self.cells = RowCells::Active(cells);
        }
        let RowCells::Active(cells) = &mut self.cells else {
            unreachable!()
        };
        cells
    }

    pub(crate) fn write_contents(&self, contents: &mut String, start: u16, width: u16, wrapping: bool) {
        let mut prev_was_wide = false;

        let mut prev_col = start;
        for (col, cell) in self
            .cells()
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            if cell.has_contents() {
                for _ in 0..(col - prev_col) {
                    contents.push(' ');
                }
                prev_col += col - prev_col;

                contents.push_str(cell.contents());
                prev_col += if cell.is_wide() { 2 } else { 1 };
            }
        }
        if prev_col == start && wrapping {
            contents.push('\n');
        }
    }

    pub(crate) fn write_contents_formatted(
        &self,
        contents: &mut Vec<u8>,
        start: u16,
        width: u16,
        row: u16,
        wrapping: bool,
        prev_pos: Option<crate::grid::Pos>,
        prev_attrs: Option<crate::attrs::Attrs>,
    ) -> (crate::grid::Pos, crate::attrs::Attrs) {
        let mut prev_was_wide = false;
        let default_cell = crate::Cell::new();

        let mut prev_pos = prev_pos.unwrap_or_else(|| {
            if wrapping {
                crate::grid::Pos {
                    row: row - 1,
                    col: self.cols(),
                }
            } else {
                crate::grid::Pos { row, col: start }
            }
        });
        let mut prev_attrs = prev_attrs.unwrap_or_default();

        let first_cell = self.get(start).unwrap();
        if wrapping && first_cell == default_cell {
            let default_attrs = default_cell.attrs();
            if &prev_attrs != default_attrs {
                default_attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = *default_attrs;
            }
            contents.push(b' ');
            crate::term::Backspace.write_buf(contents);
            crate::term::EraseChar::new(1).write_buf(contents);
            prev_pos = crate::grid::Pos { row, col: 0 };
        }

        let mut erase: Option<(u16, crate::attrs::Attrs)> = None;
        for (col, cell) in self
            .cells()
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            let pos = crate::grid::Pos { row, col };

            if let Some((prev_col, attrs)) = erase {
                if cell.has_contents() || cell.attrs() != &attrs {
                    let new_pos = crate::grid::Pos { row, col: prev_col };
                    if wrapping && prev_pos.row + 1 == new_pos.row && prev_pos.col >= self.cols() {
                        if new_pos.col > 0 {
                            contents.extend(" ".repeat(usize::from(new_pos.col)).as_bytes());
                        } else {
                            contents.extend(b" ");
                            crate::term::Backspace.write_buf(contents);
                        }
                    } else {
                        crate::term::MoveFromTo::new(prev_pos, new_pos).write_buf(contents);
                    }
                    prev_pos = new_pos;
                    if prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = attrs;
                    }
                    crate::term::EraseChar::new(pos.col - prev_col).write_buf(contents);
                    erase = None;
                }
            }

            if cell != default_cell {
                let attrs = *cell.attrs();
                if cell.has_contents() {
                    if pos != prev_pos {
                        if !wrapping
                            || prev_pos.row + 1 != pos.row
                            || prev_pos.col < self.cols() - u16::from(cell.is_wide())
                            || pos.col != 0
                        {
                            crate::term::MoveFromTo::new(prev_pos, pos).write_buf(contents);
                        }
                        prev_pos = pos;
                    }

                    if prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = attrs;
                    }

                    prev_pos.col += if cell.is_wide() { 2 } else { 1 };
                    let cell_contents = cell.contents();
                    contents.extend(cell_contents.as_bytes());
                } else if erase.is_none() {
                    erase = Some((pos.col, attrs));
                }
            }
        }
        if let Some((prev_col, attrs)) = erase {
            let new_pos = crate::grid::Pos { row, col: prev_col };
            if wrapping && prev_pos.row + 1 == new_pos.row && prev_pos.col >= self.cols() {
                if new_pos.col > 0 {
                    contents.extend(" ".repeat(usize::from(new_pos.col)).as_bytes());
                } else {
                    contents.extend(b" ");
                    crate::term::Backspace.write_buf(contents);
                }
            } else {
                crate::term::MoveFromTo::new(prev_pos, new_pos).write_buf(contents);
            }
            prev_pos = new_pos;
            if prev_attrs != attrs {
                attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = attrs;
            }
            crate::term::ClearRowForward.write_buf(contents);
        }

        (prev_pos, prev_attrs)
    }

    // while it's true that most of the logic in this is identical to
    // write_contents_formatted, i can't figure out how to break out the
    // common parts without making things noticeably slower.
    pub(crate) fn write_contents_diff(
        &self,
        contents: &mut Vec<u8>,
        prev: &Self,
        start: u16,
        width: u16,
        row: u16,
        wrapping: bool,
        prev_wrapping: bool,
        mut prev_pos: crate::grid::Pos,
        mut prev_attrs: crate::attrs::Attrs,
    ) -> (crate::grid::Pos, crate::attrs::Attrs) {
        let mut prev_was_wide = false;

        let first_cell = self.get(start).unwrap();
        let prev_first_cell = prev.get(start).unwrap();
        if wrapping
            && !prev_wrapping
            && first_cell == prev_first_cell
            && prev_pos.row + 1 == row
            && prev_pos.col >= self.cols() - u16::from(prev_first_cell.is_wide())
        {
            let first_cell_attrs = first_cell.attrs();
            if &prev_attrs != first_cell_attrs {
                first_cell_attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = *first_cell_attrs;
            }
            let mut cell_contents = prev_first_cell.contents();
            let need_erase = if cell_contents.is_empty() {
                cell_contents = " ";
                true
            } else {
                false
            };
            contents.extend(cell_contents.as_bytes());
            crate::term::Backspace.write_buf(contents);
            if prev_first_cell.is_wide() {
                crate::term::Backspace.write_buf(contents);
            }
            if need_erase {
                crate::term::EraseChar::new(1).write_buf(contents);
            }
            prev_pos = crate::grid::Pos { row, col: 0 };
        }

        let mut erase: Option<(u16, crate::attrs::Attrs)> = None;
        for (col, (cell, prev_cell)) in self
            .cells()
            .zip(prev.cells())
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            let pos = crate::grid::Pos { row, col };

            if let Some((prev_col, attrs)) = erase {
                if cell.has_contents() || cell.attrs() != &attrs {
                    let new_pos = crate::grid::Pos { row, col: prev_col };
                    if wrapping && prev_pos.row + 1 == new_pos.row && prev_pos.col >= self.cols() {
                        if new_pos.col > 0 {
                            contents.extend(" ".repeat(usize::from(new_pos.col)).as_bytes());
                        } else {
                            contents.extend(b" ");
                            crate::term::Backspace.write_buf(contents);
                        }
                    } else {
                        crate::term::MoveFromTo::new(prev_pos, new_pos).write_buf(contents);
                    }
                    prev_pos = new_pos;
                    if prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = attrs;
                    }
                    crate::term::EraseChar::new(pos.col - prev_col).write_buf(contents);
                    erase = None;
                }
            }

            if cell != prev_cell {
                let attrs = *cell.attrs();
                if cell.has_contents() {
                    if pos != prev_pos {
                        if !wrapping
                            || prev_pos.row + 1 != pos.row
                            || prev_pos.col < self.cols() - u16::from(cell.is_wide())
                            || pos.col != 0
                        {
                            crate::term::MoveFromTo::new(prev_pos, pos).write_buf(contents);
                        }
                        prev_pos = pos;
                    }

                    if prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = attrs;
                    }

                    prev_pos.col += if cell.is_wide() { 2 } else { 1 };
                    contents.extend(cell.contents().as_bytes());
                } else if erase.is_none() {
                    erase = Some((pos.col, attrs));
                }
            }
        }
        if let Some((prev_col, attrs)) = erase {
            let new_pos = crate::grid::Pos { row, col: prev_col };
            if wrapping && prev_pos.row + 1 == new_pos.row && prev_pos.col >= self.cols() {
                if new_pos.col > 0 {
                    contents.extend(" ".repeat(usize::from(new_pos.col)).as_bytes());
                } else {
                    contents.extend(b" ");
                    crate::term::Backspace.write_buf(contents);
                }
            } else {
                crate::term::MoveFromTo::new(prev_pos, new_pos).write_buf(contents);
            }
            prev_pos = new_pos;
            if prev_attrs != attrs {
                attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = attrs;
            }
            crate::term::ClearRowForward.write_buf(contents);
        }

        // if this row is going from wrapped to not wrapped, we need to erase
        // and redraw the last character to break wrapping. if this row is
        // wrapped, we need to redraw the last character without erasing it to
        // position the cursor after the end of the line correctly so that
        // drawing the next line can just start writing and be wrapped.
        if (!self.wrapped && prev.wrapped) || (!prev.wrapped && self.wrapped) {
            let end_pos = if self.get(self.cols() - 1).unwrap().is_wide_continuation() {
                crate::grid::Pos {
                    row,
                    col: self.cols() - 2,
                }
            } else {
                crate::grid::Pos {
                    row,
                    col: self.cols() - 1,
                }
            };
            crate::term::MoveFromTo::new(prev_pos, end_pos).write_buf(contents);
            prev_pos = end_pos;
            if !self.wrapped {
                crate::term::EraseChar::new(1).write_buf(contents);
            }
            let end_cell = self.get(end_pos.col).unwrap();
            if end_cell.has_contents() {
                let attrs = end_cell.attrs();
                if &prev_attrs != attrs {
                    attrs.write_escape_code_diff(contents, &prev_attrs);
                    prev_attrs = *attrs;
                }
                contents.extend(end_cell.contents().as_bytes());
                prev_pos.col += if end_cell.is_wide() { 2 } else { 1 };
            }
        }

        (prev_pos, prev_attrs)
    }
}

impl Iterator for Cells<'_> {
    type Item = crate::Cell;

    fn next(&mut self) -> Option<Self::Item> {
        if self.col >= self.row.cols {
            return None;
        }
        let cell = match &self.row.cells {
            RowCells::Active(cells) => cells[usize::from(self.col)].clone(),
            RowCells::Compact(cells) if self.col >= cells.cells => crate::Cell::new(),
            RowCells::Compact(cells) => {
                if self.run_remaining == 0 {
                    let shape = &cells.data[..usize::try_from(cells.shape_len).unwrap()];
                    self.run_remaining = u16::from(shape[self.shape_index]);
                    self.run_len = shape[self.shape_index + 1];
                    self.shape_index += 2;
                }
                let content_len = usize::from(self.run_len & crate::cell::LEN_BITS);
                let content_end = self.content_start + content_len;
                let spans: &[AttrSpan] = cells.attrs.as_ref().map_or(&[], |attrs| attrs.as_slice());
                while self.attr_index < spans.len() && spans[self.attr_index].end <= self.col {
                    self.attr_index += 1;
                }
                let attrs = spans
                    .get(self.attr_index)
                    .filter(|span| self.col >= span.start)
                    .map_or_else(crate::attrs::Attrs::default, |span| span.attrs);
                let cell = crate::Cell::from_compact(
                    &cells.data[usize::try_from(cells.shape_len).unwrap() + self.content_start
                        ..usize::try_from(cells.shape_len).unwrap() + content_end],
                    self.run_len,
                    attrs,
                );
                self.content_start = content_end;
                self.run_remaining -= 1;
                cell
            }
        };
        self.col += 1;
        Some(cell)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::from(self.row.cols - self.col);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for Cells<'_> {}

fn encode_attrs(output: &mut Vec<u8>, attrs: crate::attrs::Attrs) {
    encode_color(output, attrs.fgcolor);
    encode_color(output, attrs.bgcolor);
    encode_color(output, attrs.underline_color);
    output.push(attrs.mode);
}

fn decode_attrs(input: &mut &[u8]) -> crate::attrs::Attrs {
    crate::attrs::Attrs {
        fgcolor: decode_color(input),
        bgcolor: decode_color(input),
        underline_color: decode_color(input),
        mode: take(input, 1)[0],
    }
}

fn encode_color(output: &mut Vec<u8>, color: crate::attrs::Color) {
    match color {
        crate::attrs::Color::Default => output.extend_from_slice(&[0, 0, 0, 0]),
        crate::attrs::Color::Idx(index) => output.extend_from_slice(&[1, index, 0, 0]),
        crate::attrs::Color::Rgb(red, green, blue) => {
            output.extend_from_slice(&[2, red, green, blue]);
        }
    }
}

fn decode_color(input: &mut &[u8]) -> crate::attrs::Color {
    let color = take(input, 4);
    match color[0] {
        0 => crate::attrs::Color::Default,
        1 => crate::attrs::Color::Idx(color[1]),
        2 => crate::attrs::Color::Rgb(color[1], color[2], color[3]),
        _ => unreachable!("invalid color in an internal scrollback block"),
    }
}

fn read_u16(input: &mut &[u8]) -> u16 {
    u16::from_le_bytes(take(input, 2).try_into().unwrap())
}

fn read_u32(input: &mut &[u8]) -> u32 {
    u32::from_le_bytes(take(input, 4).try_into().unwrap())
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> &'a [u8] {
    let (value, rest) = input.split_at(len);
    *input = rest;
    value
}
