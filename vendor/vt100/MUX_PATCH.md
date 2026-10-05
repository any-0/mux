This is `vt100` 0.16.2 from crates.io with the following mux changes.

## Scrollback

- A scroll region whose top margin is the first row still sends removed rows
  to scrollback (Codex inserts resumed transcript lines above its viewport this
  way); regions starting lower, such as editor and status regions, do not.
- Rows entering scrollback are packed losslessly: text stored contiguously,
  cell shapes run-length encoded, non-default attributes kept as spans. Every
  128 rows form a block compressed with zstd (kept raw if that is no smaller);
  the newest partial block stays as individual rows. Cold blocks are decoded on
  demand and dropped again on returning to the live screen.
- The mux daemon can spill blocks to an unlinked backing file. One background
  writer with a fixed job and byte budget does the writing, so terminal
  processing never waits for storage: when the budget is full, blocks stay in
  memory. File extents are reused once no block (in a clone or snapshot
  either) refers to them, so steady-state scrollback does not grow the file.
- `encode_history`/`restore_history` persist scrollback in the same packed
  form. Restoring validates every row and streams one bounded row at a time.
  A row's span gains an 18th byte (flag bit 1) only when it uses the extended
  renditions below, so history written before them still reads the same.

## Emulation

Checked against alacritty, and against xterm and tmux where those disagree:

- DEC Special Graphics and UK character sets in G0/G1, SI/SO, saved by DECSC.
- REP (bounded to a screenful), IRM, DECAWM off, tab stops (HTS, TBC, CHT,
  CBT), IND, NEL, HPA, HPR, VPR, `CSI s`/`CSI u`, DECSTR, ED 3, modes 1047,
  1048 and 1004 (recorded so mux can report focus).
- SGR 5/6, 8, 9, 21 and 53 with their resets, colon forms of 38/48/58, and
  bold and faint as independent bits (SGR 22 clears both; formatted output
  resets intensity before setting a new combination).
- Blanks from scrolling, inserting and deleting take the current background
  (BCE); erasing keeps only the background.
- Cursor movement, but not a line feed or an edit, ends a pending wrap; a tab
  at the right margin keeps it. IL/DL act only inside the scroll region, VPA
  honours origin mode, RI above the region does not scroll it, and a DECSTBM
  of fewer than two rows is ignored.
- Both buffers share one cursor, margins and origin mode; DECSC is saved per
  buffer. A second `?1049h` does nothing. RIS keeps the scrollback.
- Every counted CSI is bounded by what it can affect (`CSI 65535 @` used to
  freeze the daemon).

## Resizing and wide characters

- The normal screen resizes like tmux: a new width rewraps history and screen
  together with the cursor kept in its text; a new height drops rows below the
  cursor before pushing rows into history, and growing pulls at most that many
  back. Whether the newest history row continues onto the screen is tracked
  explicitly, so text printed after a `clear` never joins the line before it.
  The alternate screen is only cropped.
- Both halves of a wide character stay together, except on a one-column
  screen, which keeps them on consecutive rows so widening rejoins them.
  Whatever splits a pair blanks both halves; a broken pair is never a reason
  to panic, and a combining mark never attaches to a second half.

Unused upstream API (`rows_formatted`, `rows_diff`, `contents_between`,
`input_mode_*`, `attributes_formatted`, `cursor_state_formatted`,
`application_keypad`, the screen's own attribute getters, and `Parser`'s
`Default` and `io::Write`) was removed.
