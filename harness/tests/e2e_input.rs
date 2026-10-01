//! What a program in a pane receives for each key a real terminal sends.
//! Every case goes through the real client and daemon and is captured byte
//! for byte by `dd` reading the pane's tty in raw mode.

use std::{
    fs, thread,
    time::{Duration, Instant},
};

use mux_harness::{
    escape,
    rig::{Client, Rig, TerminalOptions},
};

/// `(what the terminal sends, what the pane must get)`, in normal cursor
/// mode. Bindings mux reserves (Alt-a, Alt-s, ...) are left out.
const NORMAL: &[(&[u8], &[u8])] = &[
    (b"a", b"a"),
    ("é".as_bytes(), "é".as_bytes()),
    ("日本".as_bytes(), "日本".as_bytes()),
    (b"\r", b"\r"),
    (b"\t", b"\t"),
    (b"\x7f", b"\x7f"),
    (b"\x08", b"\x08"),
    (b"\x00", b"\x00"),
    (b"\x01", b"\x01"),
    (b"\x03", b"\x03"),
    (b"\x0a", b"\x0a"),
    (b"\x1a", b"\x1a"),
    // Ctrl-\, Ctrl-], Ctrl-^ and Ctrl-_ (also Ctrl-/): crossterm reports these
    // as Ctrl-4 through Ctrl-7.
    (b"\x1c", b"\x1c"),
    (b"\x1d", b"\x1d"),
    (b"\x1e", b"\x1e"),
    (b"\x1f", b"\x1f"),
    (b"\x1bx", b"\x1bx"),
    (b"\x1b\x18", b"\x1b\x18"),
    (b"\x1b\r", b"\x1b\r"),
    (b"\x1b\x7f", b"\x1b\x7f"),
    (b"\x1b[A", b"\x1b[A"),
    (b"\x1b[D", b"\x1b[D"),
    (b"\x1b[1;5C", b"\x1b[1;5C"),
    (b"\x1b[1;2A", b"\x1b[1;2A"),
    (b"\x1b[1;3B", b"\x1b[1;3B"),
    (b"\x1b[H", b"\x1b[H"),
    (b"\x1b[F", b"\x1b[F"),
    (b"\x1b[1~", b"\x1b[H"),
    (b"\x1b[4~", b"\x1b[F"),
    (b"\x1b[2~", b"\x1b[2~"),
    (b"\x1b[3~", b"\x1b[3~"),
    (b"\x1b[5~", b"\x1b[5~"),
    (b"\x1b[6~", b"\x1b[6~"),
    (b"\x1b[3;5~", b"\x1b[3;5~"),
    (b"\x1b[Z", b"\x1b[Z"),
    (b"\x1bOP", b"\x1bOP"),
    (b"\x1bOS", b"\x1bOS"),
    (b"\x1b[15~", b"\x1b[15~"),
    (b"\x1b[24~", b"\x1b[24~"),
    (b"\x1b[1;5P", b"\x1b[1;5P"),
];

/// The same keys in application cursor mode (DECCKM, `smkx`), which is what
/// curses programs switch on and what `khome`/`kend` in terminfo assume.
const APPLICATION: &[(&[u8], &[u8])] = &[
    (b"\x1b[A", b"\x1bOA"),
    (b"\x1bOA", b"\x1bOA"),
    (b"\x1b[C", b"\x1bOC"),
    (b"\x1b[H", b"\x1bOH"),
    (b"\x1b[F", b"\x1bOF"),
    (b"\x1b[1;5A", b"\x1b[1;5A"),
];

fn capture(rig: &Rig, client: &mut Client, setup: &[u8], cases: &[(&[u8], &[u8])]) -> Vec<String> {
    let expected: Vec<u8> = cases
        .iter()
        .flat_map(|(_, want)| want.iter().copied())
        .collect();
    let captured = rig.root.join("captured");
    let _ = fs::remove_file(&captured);
    let setup = rig.file("setup", setup);
    let ready = rig.root.join("ready");
    let _ = fs::remove_file(&ready);
    client.type_str(&format!(
        "stty raw -echo; cat '{}'; touch '{}'; dd bs=1 count={} of='{}' 2>/dev/null; stty sane\r",
        setup.display(),
        ready.display(),
        expected.len(),
        captured.display()
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "pane never got ready");
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(100));
    for (send, _) in cases {
        client.type_bytes(send);
        // Each key in a read of its own, as a person types.
        thread::sleep(Duration::from_millis(40));
        client.pump(Duration::from_millis(1));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got = fs::read(&captured).unwrap_or_default();
        if got.len() >= expected.len() || Instant::now() > deadline {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // A key that produced the wrong number of bytes shifts every later one,
    // so report the whole alignment.
    let got = fs::read(&captured).unwrap_or_default();
    let mut failures = Vec::new();
    if got != expected {
        let mut offset = 0;
        for (send, want) in cases {
            let slice = got.get(offset..offset + want.len()).unwrap_or(&[]);
            if slice != *want {
                failures.push(format!(
                    "sent {} expected {} got {}",
                    escape(send),
                    escape(want),
                    escape(slice)
                ));
            }
            offset += want.len();
        }
        failures.push(format!("whole capture: {}", escape(&got)));
    }
    failures
}

#[test]
fn keys_reach_the_pane_as_the_terminal_sent_them() {
    let rig = Rig::new();
    let mut client = rig.attach(TerminalOptions::default());
    client.wait_text("$");
    let mut failures = capture(&rig, &mut client, b"", NORMAL);
    client.settle(Duration::from_millis(100), Duration::from_secs(2));
    failures.extend(capture(&rig, &mut client, b"\x1b[?1h\x1b=", APPLICATION));
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
