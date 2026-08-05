This is `vt100` 0.16.2 from crates.io.

Mux changes one rule in `Grid::scroll_up`: a scroll region whose top margin is
the first terminal row still contributes removed rows to scrollback. Codex uses
that terminal behavior when inserting resumed transcript lines above its live
viewport. Regions that start below the first row, such as editor and status
regions, remain excluded from scrollback.

Rows are also converted to a lossless compact representation when they enter
scrollback. UTF-8 cell contents are stored contiguously, repeated cell shapes
are run-length encoded, default attributes take no space, and non-default
attributes are stored as spans. Groups of 256 immutable rows are then encoded
together and compressed independently with zstd; incompressible blocks retain
the smaller terminal encoding directly. The newest partial block remains as
individual rows.

Reading a cold block decodes it on demand. Returning to the live screen drops
decoded blocks, while resizing streams every row through reflow and immediately
rebuilds compact blocks. Active screen rows keep the original mutable cell
vectors. Reading, cloning, reflowing, and restoring scrollback exposes the same
cells as before, including wide and combining characters, wrapping, colors, and
styled blank cells.
