use std::{
    env, fs,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    config::Settings,
    protocol::{ClientMessage, Hello, MuxQuery, ServerMessage, read_message, write_message},
};

struct TestDaemon(Child);

impl TestDaemon {
    fn start(socket: &Path, home: &Path, state_home: &Path) -> Self {
        let child = Command::new(env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "server::lifecycle_tests::daemon_worker",
                "--nocapture",
            ])
            .env("MUX_TEST_DAEMON_SOCKET", socket)
            .env("HOME", home)
            .env("XDG_STATE_HOME", state_home)
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

    fn wait(mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "test daemon exited with {status}");
                return;
            }
            assert!(Instant::now() < deadline, "test daemon did not stop");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn connect(socket: &Path) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                return stream;
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("connect to test daemon: {error}"),
        }
    }
}

fn hello(cwd: PathBuf) -> ClientMessage {
    let settings = Settings::default();
    ClientMessage::Hello(Box::new(Hello {
        cols: 80,
        rows: 24,
        cwd,
        session: Some("lifecycle".into()),
        bindings: settings.bindings,
        clipboard_command: settings.clipboard_command,
        terminal_clipboard: false,
        theme: settings.theme,
        theme_command: settings.theme_command,
        theme_directory: settings.theme_directory,
        mouse: settings.mouse,
        bell_style: settings.bell_style,
        terminal: crate::frame::TerminalFeatures::FULL,
        glyphs: crate::config::Glyphs::Font,
        default_cursor_shape: settings.default_cursor_shape,
    }))
}

fn shutdown(socket: &Path) {
    let mut stream = connect(socket);
    write_message(&mut stream, &ClientMessage::Shutdown).unwrap();
    assert!(matches!(
        read_message(&mut stream).unwrap(),
        Some(ServerMessage::Detached)
    ));
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
fn daemon_restarts_from_durable_state() {
    let root = env::temp_dir().join(format!("mux-lifecycle-{}", std::process::id()));
    let home = root.join("home");
    let state_home = root.join("state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&state_home).unwrap();
    let socket = root.join("daemon").join("mux.sock");

    let first = TestDaemon::start(&socket, &home, &state_home);
    let mut attached = connect(&socket);
    write_message(&mut attached, &hello(root.clone())).unwrap();
    assert!(matches!(
        read_message(&mut attached).unwrap(),
        Some(ServerMessage::Render(_))
    ));
    shutdown(&socket);
    first.wait();

    let state_file = state_home.join("mux/state.bin");
    let first_state = super::decode_persisted_state(&fs::read(&state_file).unwrap()).unwrap();
    assert_eq!(first_state.sessions.len(), 1);
    assert_eq!(first_state.sessions[0].windows[0].panes.len(), 1);
    let pane_id = first_state.sessions[0].windows[0].panes[0].id;
    assert!(
        state_home
            .join(format!("mux/pane-{pane_id}.ansi"))
            .is_file()
    );

    let second = TestDaemon::start(&socket, &home, &state_home);
    let mut query = connect(&socket);
    write_message(
        &mut query,
        &ClientMessage::Query {
            pane_id: None,
            query: MuxQuery::Sessions,
            json: false,
        },
    )
    .unwrap();
    let Some(ServerMessage::Listing(sessions)) = read_message(&mut query).unwrap() else {
        panic!("restarted daemon did not answer session query");
    };
    assert!(sessions.iter().any(|session| session.contains("lifecycle")));
    shutdown(&socket);
    second.wait();

    let second_state = super::decode_persisted_state(&fs::read(&state_file).unwrap()).unwrap();
    assert_eq!(second_state.next_session_id, first_state.next_session_id);
    assert_eq!(second_state.next_pane_id, first_state.next_pane_id);
    let _ = fs::remove_dir_all(root);
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

#[test]
fn real_pty_history_and_styles_survive_restart_and_a_corrupt_sibling() {
    use super::{JOURNAL_HISTORY, encode_journal_record};
    use crate::protocol::MuxCommand;

    let root = env::temp_dir().join(format!("mux-history-lifecycle-{}", std::process::id()));
    let home = root.join("home");
    let state_home = root.join("state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&state_home).unwrap();
    let socket = root.join("daemon/mux.sock");
    let first = TestDaemon::start(&socket, &home, &state_home);
    let mut attached = connect(&socket);
    write_message(&mut attached, &hello(root.clone())).unwrap();
    let mut terminal = vt100::Parser::new(24, 80, 0);
    let first_output = root.join("first-output");
    fs::write(&first_output, b"\x1bcFIRST-PANE").unwrap();
    write_message(
        &mut attached,
        &ClientMessage::Paste(format!("stty -echo; cat '{}'\n", first_output.display())),
    )
    .unwrap();
    wait_for_render(&mut attached, &mut terminal, "FIRST-PANE");
    write_message(
        &mut attached,
        &ClientMessage::Command {
            pane_id: None,
            command: MuxCommand::NewWindow,
        },
    )
    .unwrap();
    let history_output = root.join("history-output");
    let mut history = String::new();
    for line in 0..40 {
        history.push_str(&format!("HISTORY-{line:02}\r\n"));
    }
    history.push_str("\x1b[4:3;58;5;45mSTYLED-LAST\x1b[24;59m PLAIN-LAST");
    fs::write(&history_output, history).unwrap();
    // Markers only occur in PTY output, never in the echoed shell command.
    write_message(
        &mut attached,
        &ClientMessage::Paste(format!("stty -echo; cat '{}'\n", history_output.display())),
    )
    .unwrap();
    wait_for_render(&mut attached, &mut terminal, "PLAIN-LAST");
    shutdown(&socket);
    first.wait();

    let state_path = state_home.join("mux/state.bin");
    let state = super::decode_persisted_state(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(state.sessions[0].windows.len(), 2);
    let damaged = state.sessions[0].windows[0].panes[0].id;
    let healthy = state.sessions[0].windows[1].panes[0].id;
    let healthy_path = state_home.join(format!("mux/pane-{healthy}.ansi"));
    let mut saved = super::new_parser(24, 80);
    super::replay_pane_journal(&mut saved, fs::File::open(&healthy_path).unwrap()).unwrap();
    let (buffer, _) = super::snapshot_screen(saved.screen_mut());
    assert!(buffer.texts().any(|line| line.contains("HISTORY-00")));

    // A fully framed but malformed history row used to panic in Row::decode
    // on a restore worker, taking down startup of every saved session.
    let mut invalid = 1u32.to_le_bytes().to_vec();
    invalid.extend(1u64.to_le_bytes());
    invalid.extend([0, 0]);
    fs::write(
        state_home.join(format!("mux/pane-{damaged}.ansi")),
        encode_journal_record(JOURNAL_HISTORY, &invalid).unwrap(),
    )
    .unwrap();
    // A crash can also leave any partial header/payload behind a healthy pane.
    let mut healthy_bytes = fs::read(&healthy_path).unwrap();
    healthy_bytes.extend([1, 0, 0, 0, 5, b'x']);
    fs::write(&healthy_path, healthy_bytes).unwrap();

    let second = TestDaemon::start(&socket, &home, &state_home);
    let mut attached = connect(&socket);
    write_message(&mut attached, &hello(root.clone())).unwrap();
    let mut restored = vt100::Parser::new(24, 80, 0);
    wait_for_render(&mut attached, &mut restored, "STYLED-LAST");
    assert!(restored.screen().contents().contains("PLAIN-LAST"));
    let mut styled = 0;
    for row in 0..24 {
        for col in 0..80 {
            let cell = restored.screen().cell(row, col).unwrap();
            if cell.underline_style() == vt100::UnderlineStyle::Curly {
                assert_eq!(cell.underline_color(), vt100::Color::Idx(45));
                styled += 1;
            }
        }
    }
    assert_eq!(styled, "STYLED-LAST".len());
    write_message(
        &mut attached,
        &ClientMessage::Command {
            pane_id: None,
            command: MuxCommand::SelectWindow(1),
        },
    )
    .unwrap();
    let usable_output = root.join("usable-output");
    fs::write(&usable_output, b"CORRUPT-PANE-STILL-USABLE").unwrap();
    write_message(
        &mut attached,
        &ClientMessage::Paste(format!("stty -echo; cat '{}'\n", usable_output.display())),
    )
    .unwrap();
    wait_for_render(&mut attached, &mut restored, "CORRUPT-PANE-STILL-USABLE");
    shutdown(&socket);
    second.wait();
    let final_state = super::decode_persisted_state(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(final_state.next_pane_id, state.next_pane_id);
    assert_eq!(final_state.sessions[0].windows.len(), 2);
    fs::remove_dir_all(root).unwrap();
}
