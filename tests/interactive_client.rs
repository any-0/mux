//! Exercise crossterm and the real executable inside a terminal, not only the
//! daemon protocol. Generated fixtures keep output markers out of typed input.
use std::{
    env, fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

struct TerminalSession {
    root: PathBuf,
    daemon: std::process::Child,
    client: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    input: Box<dyn Write + Send>,
    output: Receiver<Vec<u8>>,
    terminal: vt100::Parser,
}

impl TerminalSession {
    fn start() -> Self {
        let root = env::temp_dir().join(format!("mux-interactive-{}", std::process::id()));
        let runtime = root.join("runtime");
        let home = root.join("home");
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(&home).unwrap();
        let config = root.join("config.toml");
        fs::write(&config, "mouse = true\n").unwrap();
        let socket = runtime.join("mux.sock");
        let daemon = Command::new(env!("CARGO_BIN_EXE_mux"))
            .arg("__server")
            .arg(&socket)
            .env("HOME", &home)
            .env("SHELL", "/bin/sh")
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env_remove("MUX")
            .env_remove("MUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 12,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_mux"));
        command.args([
            "--config",
            config.to_str().unwrap(),
            "--session",
            "interactive",
        ]);
        command.cwd(&root);
        command.env("HOME", &home);
        command.env("SHELL", "/bin/sh");
        command.env("XDG_STATE_HOME", root.join("state"));
        command.env("XDG_RUNTIME_DIR", &runtime);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env_remove("MUX");
        command.env_remove("MUX_PANE");
        // Starting the client after bind avoids testing the separate auto-start
        // race and guarantees this guard owns the only daemon process.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "daemon did not bind");
            thread::sleep(Duration::from_millis(10));
        }
        let client = pair.slave.spawn_command(command).unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let input = pair.master.take_writer().unwrap();
        let (sender, output) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = [0; 8192];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) | Err(_) => return,
                    Ok(length) => {
                        if sender.send(bytes[..length].to_vec()).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut session = Self {
            root,
            daemon,
            client,
            master: pair.master,
            input,
            output,
            terminal: vt100::Parser::new(12, 40, 0),
        };
        session.wait(|screen| screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None);
        session
    }

    fn wait(&mut self, ready: impl Fn(&vt100::Screen) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready(self.terminal.screen()) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "terminal timed out: {:?}",
                self.terminal.screen().contents()
            );
            let bytes = self
                .output
                .recv_timeout(remaining)
                .expect("client produced no terminal output");
            self.terminal.process(&bytes);
        }
    }

    fn type_bytes(&mut self, bytes: &[u8]) {
        self.input.write_all(bytes).unwrap();
        self.input.flush().unwrap();
    }

    fn fixture(&mut self, name: &str, bytes: &[u8], marker: &str) {
        let path = self.root.join(name);
        fs::write(&path, bytes).unwrap();
        self.type_bytes(format!("stty -echo; cat '{}'\r", path.display()).as_bytes());
        self.wait(|screen| screen.contents().contains(marker));
    }

    fn command(&self, command: &str) {
        let result = Command::new(env!("CARGO_BIN_EXE_mux"))
            .arg(command)
            .env("MUX", self.root.join("runtime/mux.sock"))
            .env_remove("MUX_PANE")
            .output()
            .unwrap();
        assert!(result.status.success(), "command failed: {:?}", result);
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.client.kill();
        let _ = self.client.wait();
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn executable_decodes_keys_bracketed_paste_and_mouse_then_selects_visible_windows() {
    let mut session = TerminalSession::start();
    session.fixture("first", b"\x1bcFIRST-WINDOW", "FIRST-WINDOW");
    let setup = session.root.join("setup");
    let done = session.root.join("done");
    let captured = session.root.join("input-bytes");
    fs::write(
        &setup,
        b"\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h\r\nINPUT-READY",
    )
    .unwrap();
    fs::write(
        &done,
        b"\x1b[?1l\x1b[?2004l\x1b[?1002l\x1b[?1006l\r\nINPUT-DONE",
    )
    .unwrap();
    let expected = b"\x1bOA\x1b[200~hello\nworld\x1b[201~\x1b[<0;5;3M\x1b[<0;5;3m";
    session.type_bytes(format!("stty raw -echo; cat '{}'; dd bs=1 count={} of='{}' 2>/dev/null; stty -raw -echo; cat '{}'\r", setup.display(), expected.len(), captured.display(), done.display()).as_bytes());
    session.wait(|screen| screen.contents().contains("INPUT-READY"));
    session.type_bytes(b"\x1b[A\x1b[200~hello\nworld\x1b[201~\x1b[<0;10;3M\x1b[<0;10;3m");
    session.wait(|screen| screen.contents().contains("INPUT-DONE"));
    assert_eq!(fs::read(&captured).unwrap(), expected);
    session.command("new-window");
    session.fixture("second", b"\x1bcSECOND-WINDOW", "SECOND-WINDOW");
    session.command("new-window");
    session.fixture("third", b"\x1bcTHIRD-WINDOW", "THIRD-WINDOW");
    session
        .master
        .resize(PtySize {
            rows: 7,
            cols: 40,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    session.terminal.screen_mut().set_size(7, 40);
    session.wait(|screen| screen.cell(2, 1).unwrap().contents() == "•");
    session.type_bytes(b"\x1b[<0;2;3M\x1b[<0;2;3m");
    // Execute a command after the click. Its unique output proves which real
    // shell received subsequent input, even when a click produces no redraw.
    session.fixture("clicked", b"\r\nCLICK-READY", "CLICK-READY");
    assert!(
        session
            .terminal
            .screen()
            .contents()
            .contains("THIRD-WINDOW")
    );
}
