This is `vt100` 0.16.2 from crates.io.

Mux changes one rule in `Grid::scroll_up`: a scroll region whose top margin is
the first terminal row still contributes removed rows to scrollback. Codex uses
that terminal behavior when inserting resumed transcript lines above its live
viewport. Regions that start below the first row, such as editor and status
regions, remain excluded from scrollback.

Rows are also converted to a lossless compact representation when they enter
scrollback. UTF-8 cell contents are stored contiguously, repeated cell shapes
are run-length encoded, default attributes take no space, and non-default
attributes are stored as spans. Groups of 128 immutable rows are then encoded
together and compressed independently with zstd; incompressible blocks retain
the smaller terminal encoding directly. The newest partial block remains as
individual rows.

Reading a cold block decodes it on demand. Returning to the live screen drops
decoded blocks, while resizing streams every row through reflow and immediately
rebuilds compact blocks. Active screen rows keep the original mutable cell
vectors. Reading, cloning, reflowing, and restoring scrollback exposes the same
cells as before, including wide and combining characters, wrapping, colors, and
styled blank cells.

Immutable blocks can be spilled to an unlinked backing file by the mux daemon.
The background writer has a fixed job and byte budget, so terminal processing
never waits for storage; blocks remain compressed in memory when that budget is
full. File extents are reused after their last block reference is dropped. A
cloned screen or persistence snapshot therefore keeps its extents readable, while
steady-state scrollback no longer grows the backing file as old rows expire.

## Terminal emulation

Mux extends the emulator to what the programs it hosts actually send, checked
against alacritty by a differential fuzzer in `harness/` and, where the two
disagree, against xterm and tmux:

- Character sets: DEC Special Graphics and the UK set in G0 and G1, SI/SO, and
  both saved and restored by DECSC/DECRC. Curses draws its borders with them.
- REP (repeat the last character, at most a screenful), IRM (insert mode),
  DECAWM (autowrap off overwrites the last column and drops a wide character
  that no longer fits), tab stops (HTS, TBC, CHT, CBT), IND, NEL, HPA, HPR, VPR,
  `CSI s`/`CSI u`, DECSTR (soft reset), ED 3 (erase saved lines), and the 1047
  and 1048 alternate screen modes. Mode 1004 is recorded so mux can report focus.
- SGR 5/6 (blink), 8 (hidden), 9 (strikethrough), 21 (double underline) and 53
  (overline) with their resets, and the colon forms of 38 and 48 with a color
  space field. Bold and faint combine as in xterm instead of replacing each
  other. These renditions live in a new `Attrs::extra` byte; a packed row only
  uses the longer (18-byte) span form, flagged in bit 1 of its flag byte, when
  one of its spans needs it, so history written before and read by older
  versions is unchanged.
- Blank cells made by scrolling, inserting and deleting take the current
  background (xterm's BCE), and erasing uses only the background.
- Cursor movement (but not a line feed or an edit) ends a pending wrap; a tab
  at the right margin leaves it pending, as tmux does. IL and DL only act inside
  the scroll region and reset the column; VPA honours origin mode; RI above the
  region no longer scrolls it; a DECSTBM with fewer than two rows is ignored.
- One cursor, one pair of margins and one origin mode are shared by both
  buffers, while DECSC keeps a saved cursor per buffer, as in xterm. A second
  `?1049h` does nothing.
- RIS keeps the scrollback (and its backing file), like xterm.
- Every CSI with a count is bounded by what it can affect. ICH used to insert
  one cell at a time without a bound: `CSI 65535 @` took seconds, freezing the
  daemon and every pane with it.

## Resizing

The normal screen is resized the way tmux does it. A new width rewraps the
history and the screen together, and the cursor keeps its place in the text,
including just after a line that exactly fills the width. A new height gives up
blank rows below the cursor before sending rows off the top into the history,
and a taller screen takes rows back out of it, at most as many as it grew by.
Whether the newest history row continues onto the first screen row is tracked
explicitly, because a wrap flag alone would join text printed after a `clear`
onto the line before it. The alternate screen is only cropped.

## Wide characters

Every half of a wide character has its other half beside it, except on a
one-column screen, which keeps them on consecutive rows so that widening joins
them again. Anything that splits a pair (an insertion, a deletion, a resize)
blanks both halves, and no path assumes the invariant holds: a broken pair is
blanked or skipped, never a reason to panic. A combining mark never attaches to
a continuation half.
