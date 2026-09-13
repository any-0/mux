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
        truecolor: true,
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
