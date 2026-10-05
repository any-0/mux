use std::{
    env, fs,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::{
    JOURNAL_HISTORY, PersistedState, decode_persisted_state, encode_journal_record, new_parser,
    replay_pane_journal, snapshot_screen,
    tests::{hello, scratch},
};
use crate::protocol::{ClientMessage, MuxCommand, ServerMessage, read_message, write_message};

/// A real daemon process, re-running this test binary as `daemon_worker`.
struct TestDaemon(Child);

impl TestDaemon {
    fn start(root: &Path) -> Self {
        let child = Command::new(env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "server::lifecycle_tests::daemon_worker",
                "--nocapture",
            ])
            .env("MUX_TEST_DAEMON_SOCKET", socket(root))
            .env("HOME", root.join("home"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("SHELL", "/bin/sh")
            .env_remove("MUX")
            .env_remove("MUX_PANE")
            .env_remove("ZDOTDIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        Self(child)
    }

    /// Asks the daemon to shut down and waits for a clean exit.
    fn stop(mut self, root: &Path) {
        let mut stream = connect(root);
        crate::protocol::write_shutdown(&mut stream).unwrap();
        assert!(matches!(
            read_message(&mut stream).unwrap(),
            Some(ServerMessage::Detached)
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "test daemon did not stop");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(self.0.wait().unwrap().success());
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn socket(root: &Path) -> PathBuf {
    root.join("daemon/mux.sock")
}

fn connect(root: &Path) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(socket(root)) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                return stream;
            }
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("connect to test daemon: {error}"),
        }
    }
}

fn attach(root: &Path) -> UnixStream {
    let mut stream = connect(root);
    write_message(&mut stream, &hello(24, 80, root.to_path_buf(), "lifecycle")).unwrap();
    stream
}

fn saved_state(root: &Path) -> PersistedState {
    decode_persisted_state(&fs::read(root.join("state/mux/state.bin")).unwrap()).unwrap()
}

fn pane_journal(root: &Path, pane_id: usize) -> PathBuf {
    root.join(format!("state/mux/pane-{pane_id}.ansi"))
}

/// Has the attached pane print `bytes`, which must end with `marker`.
fn print(
    stream: &mut UnixStream,
    terminal: &mut vt100::Parser,
    root: &Path,
    bytes: &[u8],
    marker: &str,
) {
    // The marker must appear only in PTY output, not the echoed command.
    let path = root.join(marker.to_lowercase());
    fs::write(&path, bytes).unwrap();
    let command = format!("stty -echo; cat '{}'\n", path.display());
    write_message(stream, &ClientMessage::Paste(command)).unwrap();
    wait_for_render(stream, terminal, marker);
}

fn wait_for_render(stream: &mut UnixStream, terminal: &mut vt100::Parser, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !terminal.screen().contents().contains(marker) {
        assert!(Instant::now() < deadline, "render never showed {marker:?}");
        match read_message::<ServerMessage>(stream).unwrap() {
            Some(ServerMessage::Render(bytes)) => terminal.process(&bytes),
            Some(ServerMessage::Error(error)) => panic!("daemon error: {error}"),
            Some(_) => {}
            None => panic!("daemon disconnected before {marker:?}"),
        }
    }
}

fn command(stream: &mut UnixStream, command: MuxCommand) {
    let message = ClientMessage::Command {
        pane_id: None,
        command,
    };
    write_message(stream, &message).unwrap();
}

#[test]
#[ignore]
fn daemon_worker() {
    let Some(socket) = env::var_os("MUX_TEST_DAEMON_SOCKET") else {
        return;
    };
    super::run(Path::new(&socket)).unwrap();
}

#[test]
fn real_pty_history_and_styles_survive_restart_and_a_corrupt_sibling() {
    let root = scratch("lifecycle");
    fs::create_dir_all(root.join("home")).unwrap();
    fs::create_dir_all(root.join("state")).unwrap();

    let first = TestDaemon::start(&root);
    let mut attached = attach(&root);
    let mut terminal = vt100::Parser::new(24, 80, 0);
    print(
        &mut attached,
        &mut terminal,
        &root,
        b"\x1bcFIRST-PANE",
        "FIRST-PANE",
    );
    command(&mut attached, MuxCommand::NewWindow);
    let mut history = String::new();
    for line in 0..40 {
        history.push_str(&format!("HISTORY-{line:02}\r\n"));
    }
    history.push_str("\x1b[4:3;58;5;45mSTYLED-LAST\x1b[24;59m PLAIN-LAST");
    print(
        &mut attached,
        &mut terminal,
        &root,
        history.as_bytes(),
        "PLAIN-LAST",
    );
    first.stop(&root);

    let state = saved_state(&root);
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].windows.len(), 2);
    let damaged = state.sessions[0].windows[0].panes[0].id;
    let healthy = pane_journal(&root, state.sessions[0].windows[1].panes[0].id);
    let mut saved = new_parser(24, 80);
    replay_pane_journal(&mut saved, fs::File::open(&healthy).unwrap()).unwrap();
    let (buffer, _) = snapshot_screen(saved.screen_mut());
    assert!(buffer.texts().any(|line| line.contains("HISTORY-00")));

    // A framed but malformed history row used to panic a restore worker and
    // take down startup of every saved session.
    let mut invalid = 1u32.to_le_bytes().to_vec();
    invalid.extend(1u64.to_le_bytes());
    invalid.extend([0, 0]);
    let record = encode_journal_record(JOURNAL_HISTORY, &invalid).unwrap();
    fs::write(pane_journal(&root, damaged), record).unwrap();
    // A crash can also leave a partial record behind a healthy pane.
    let mut healthy_bytes = fs::read(&healthy).unwrap();
    healthy_bytes.extend([1, 0, 0, 0, 5, b'x']);
    fs::write(&healthy, healthy_bytes).unwrap();

    let second = TestDaemon::start(&root);
    let mut attached = attach(&root);
    let mut restored = vt100::Parser::new(24, 80, 0);
    wait_for_render(&mut attached, &mut restored, "STYLED-LAST");
    assert!(restored.screen().contents().contains("PLAIN-LAST"));
    let curly: Vec<_> = (0..24)
        .flat_map(|row| (0..80).map(move |col| (row, col)))
        .filter_map(|(row, col)| restored.screen().cell(row, col))
        .filter(|cell| cell.underline_style() == vt100::UnderlineStyle::Curly)
        .map(|cell| cell.underline_color())
        .collect();
    assert_eq!(curly, vec![vt100::Color::Idx(45); "STYLED-LAST".len()]);
    // The pane whose history was corrupt still works.
    command(&mut attached, MuxCommand::SelectWindow(1));
    let marker = "CORRUPT-PANE-STILL-USABLE";
    print(
        &mut attached,
        &mut restored,
        &root,
        marker.as_bytes(),
        marker,
    );
    second.stop(&root);

    // Restoring created nothing new.
    let final_state = saved_state(&root);
    assert_eq!(final_state.next_session_id, state.next_session_id);
    assert_eq!(final_state.next_pane_id, state.next_pane_id);
    assert_eq!(final_state.sessions[0].windows.len(), 2);
    fs::remove_dir_all(root).unwrap();
}
