"""Native screen-engine check, run by the ignored Rust frame regression.

MUX_TEST_KITTY=/path/to/kitty cargo test unicode_diffs_in_kitty -- --ignored
No window, daemon, or existing terminal session is opened or changed.
"""

import json
import sys

from kitty.fast_data_types import Screen, set_options
from kitty.options.types import defaults

set_options(defaults)
fixture = json.load(sys.stdin)
screen = Screen(None, fixture["rows"], fixture["cols"], 0)
for step, frame in enumerate(fixture["frames"]):
    data = memoryview(bytes(frame["bytes"]))
    while data:
        dest = screen.test_create_write_buffer()
        consumed = screen.test_commit_write_buffer(data, dest)
        data = data[consumed:]
        screen.test_parse_written_data()
    for row, col, expected in frame["ascii"]:
        actual = screen.line(row)[col].replace("\0", " ")
        # Kitty's embedded Python runs with assertions disabled.
        if actual != expected:
            raise RuntimeError(
                f"step {step}, row {row}, col {col}: {actual!r} != {expected!r}; "
                f"line: {str(screen.line(row))!r}"
            )
print(f"Kitty verified {len(fixture['frames'])} incremental frames")
