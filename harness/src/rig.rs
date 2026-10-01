//! Drives the real `mux` binary: a daemon, and clients attached through
//! pseudo-terminals whose output is parsed by the reference emulator. What a
//! test sees is exactly what a person at that terminal would see.

use std::{
    env, fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::{reference::Reference, screen::Snapshot};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

/// The `mux` executable under test: `MUX_BIN`, or the workspace's release
/// build.
pub fn mux_binary() -> PathBuf {
    if let Some(path) = env::var_os("MUX_BIN") {
        return PathBuf::from(path);
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/release/mux");
    assert!(
        path.exists(),
        "{} is missing: run `cargo build --release` in the repository, or set MUX_BIN",
        path.display()
    );
    path
}

/// Options for the terminal a client runs in.
#[derive(Clone, Debug)]
pub struct TerminalOptions {
    pub rows: u16,
    pub cols: u16,
    pub term: String,
    pub colorterm: Option<String>,
    pub extra_env: Vec<(String, String)>,
    pub session: Option<String>,
    /// Characters this terminal draws differently from mux: each is replaced
    /// in the client's output before the terminal sees it, as a terminal that
    /// renders, say, an ambiguous-width character two columns wide would.
    pub substitute: Vec<(String, String)>,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        Self {
            rows: 24,
            cols: 80,
            term: "xterm-256color".into(),
            colorterm: Some("truecolor".into()),
            extra_env: Vec::new(),
            session: Some("test".into()),
            substitute: Vec::new(),
        }
    }
}

/// A private mux installation: its own home, state, runtime directory and
/// daemon. Dropping it kills everything it started and removes the files.
pub struct Rig {
    pub root: PathBuf,
    pub socket: PathBuf,
    daemon: Option<std::process::Child>,
    config: PathBuf,
}

impl Rig {
    pub fn new() -> Self {
        Self::with_config("mouse = true\n")
    }

    pub fn with_config(config: &str) -> Self {
        // Unix socket paths are short (104 bytes on macOS), so the root lives
        // directly under /tmp rather than the long per-user temp directory.
        let root = PathBuf::from(format!(
            "/tmp/muxh-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        let runtime = root.join("run");
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let config_path = root.join("config.toml");
        fs::write(&config_path, config).unwrap();
        let mut rig = Self {
            socket: runtime.join("mux.sock"),
            root,
            daemon: None,
            config: config_path,
        };
        rig.start_daemon();
        rig
    }

    fn apply_env(&self, command: &mut Command) {
        for (key, value) in self.env() {
            command.env(key, value);
        }
        command
            .env_remove("MUX")
            .env_remove("MUX_PANE")
            .env_remove("TMUX");
    }

    /// The environment every process of this rig runs with.
    pub fn env(&self) -> Vec<(String, String)> {
        let path = |p: &str| self.root.join(p).to_string_lossy().into_owned();
        vec![
            ("HOME".into(), path("home")),
            ("SHELL".into(), "/bin/sh".into()),
            ("XDG_STATE_HOME".into(), path("state")),
            ("XDG_CONFIG_HOME".into(), path("config")),
            ("XDG_RUNTIME_DIR".into(), path("run")),
            ("PS1".into(), "$ ".into()),
            ("ENV".into(), "/dev/null".into()),
            ("LANG".into(), "en_US.UTF-8".into()),
            ("LC_ALL".into(), "en_US.UTF-8".into()),
        ]
    }

    /// Starts (or restarts) the daemon and waits for its socket.
    pub fn start_daemon(&mut self) {
        let _ = fs::remove_file(&self.socket);
        let mut command = Command::new(mux_binary());
        command.arg("__server").arg(&self.socket);
        self.apply_env(&mut command);
        command.stdin(Stdio::null()).stdout(Stdio::null());
        command.stderr(fs::File::create(self.root.join("daemon.log")).unwrap());
        self.daemon = Some(command.spawn().expect("start daemon"));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.socket.exists() {
            assert!(Instant::now() < deadline, "daemon did not bind its socket");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kills the daemon the hard way, as a crash or `kill -9` would.
    pub fn crash_daemon(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }

    /// Stops the daemon with `mux kill-server`.
    pub fn stop_daemon(&mut self) {
        let _ = self.mux(&["kill-server"]);
        if let Some(mut daemon) = self.daemon.take() {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if daemon.try_wait().unwrap().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = daemon.kill();
            let _ = daemon.wait();
            panic!("daemon did not exit after kill-server");
        }
    }

    /// Runs a `mux` subcommand against this rig's daemon.
    pub fn mux(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(mux_binary());
        command.args(args);
        self.apply_env(&mut command);
        command.env("MUX", &self.socket);
        command.output().expect("run mux command")
    }

    /// Runs a `mux` subcommand and requires it to succeed.
    pub fn mux_ok(&self, args: &[&str]) -> String {
        let output = self.mux(args);
        assert!(
            output.status.success(),
            "mux {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The panes of the current session, parsed from `list-panes --json`.
    pub fn panes(&self) -> Vec<serde_json::Value> {
        let listing = self.mux_ok(&["list-panes", "--json"]);
        let mut panes = Vec::new();
        for line in listing.lines() {
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(serde_json::Value::Array(items)) => panes.extend(items),
                Ok(item) => panes.push(item),
                Err(_) => {}
            }
        }
        panes
    }

    pub fn attach(&self, options: TerminalOptions) -> Client {
        Client::attach(self, options)
    }

    /// What a client attaching now, at `client`'s size and session, paints
    /// from scratch. A terminal kept up to date incrementally must look the
    /// same, or its repaints have drifted from what the daemon meant.
    pub fn fresh_view(&self, client: &Client) -> Snapshot {
        let (rows, cols) = client.size();
        let mut fresh = self.attach(TerminalOptions {
            rows,
            cols,
            ..client.options.clone()
        });
        fresh.settle(Duration::from_millis(200), Duration::from_secs(5));
        let screen = fresh.screen();
        drop(fresh);
        screen
    }

    /// A file inside the rig, for fixtures a pane can `cat`.
    pub fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    pub fn daemon_log(&self) -> String {
        fs::read_to_string(self.root.join("daemon.log")).unwrap_or_default()
    }

    pub fn config_path(&self) -> &Path {
        &self.config
    }
}

impl Default for Rig {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.mux(&["kill-server"]);
        if let Some(mut daemon) = self.daemon.take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline && daemon.try_wait().ok().flatten().is_none() {
                thread::sleep(Duration::from_millis(10));
            }
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        if env::var_os("MUX_KEEP_RIG").is_none() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// One attached client and the terminal it draws into.
pub struct Client {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    input: Box<dyn Write + Send>,
    output: Receiver<Vec<u8>>,
    /// While set, the reader thread stops draining the PTY, as a stalled
    /// terminal or a frozen ssh link would.
    paused: Arc<Mutex<bool>>,
    pub terminal: Reference,
    /// Every byte the client wrote, for post-mortems.
    pub transcript: Vec<u8>,
    /// The tail of a chunk that ended inside a UTF-8 character.
    carry: Vec<u8>,
    rows: u16,
    cols: u16,
    pub options: TerminalOptions,
}

impl Client {
    fn attach(rig: &Rig, options: TerminalOptions) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: options.rows,
                cols: options.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(mux_binary());
        command.arg("--config");
        command.arg(rig.config_path());
        if let Some(session) = &options.session {
            command.args(["--session", session]);
        }
        command.cwd(rig.root.join("home"));
        for (key, value) in rig.env() {
            command.env(key, value);
        }
        command.env_remove("MUX");
        command.env_remove("MUX_PANE");
        command.env_remove("TMUX");
        command.env_remove("COLORTERM");
        command.env("TERM", &options.term);
        if let Some(colorterm) = &options.colorterm {
            command.env("COLORTERM", colorterm);
        }
        for (key, value) in &options.extra_env {
            command.env(key, value);
        }
        let child = pair.slave.spawn_command(command).expect("spawn client");
        let options = options.clone();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let input = pair.master.take_writer().unwrap();
        let (sender, output) = mpsc::channel();
        let paused = Arc::new(Mutex::new(false));
        let reader_paused = paused.clone();
        thread::spawn(move || {
            let mut bytes = vec![0; 65536];
            loop {
                while *reader_paused.lock().unwrap() {
                    thread::sleep(Duration::from_millis(5));
                }
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
        Self {
            child,
            master: pair.master,
            input,
            output,
            paused,
            terminal: Reference::new(options.rows as usize, options.cols as usize),
            transcript: Vec::new(),
            carry: Vec::new(),
            rows: options.rows,
            cols: options.cols,
            options,
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    /// Feeds whatever the client has written so far into the terminal.
    /// Returns whether anything arrived.
    pub fn pump(&mut self, wait: Duration) -> bool {
        match self.output.recv_timeout(wait) {
            Ok(bytes) => {
                self.display(bytes);
                // Keep draining without waiting while data is flowing.
                while let Ok(bytes) = self.output.try_recv() {
                    self.display(bytes);
                }
                true
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => false,
        }
    }

    fn display(&mut self, bytes: Vec<u8>) {
        self.transcript.extend_from_slice(&bytes);
        if self.options.substitute.is_empty() {
            self.terminal.feed(&bytes);
            return;
        }
        let mut pending = std::mem::take(&mut self.carry);
        pending.extend(bytes);
        // Hold back an incomplete character at the end for the next chunk.
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => pending.len(),
        };
        self.carry = pending.split_off(valid);
        let mut text = String::from_utf8_lossy(&pending).into_owned();
        for (from, to) in &self.options.substitute {
            text = text.replace(from.as_str(), to);
        }
        self.terminal.feed(text.as_bytes());
    }

    /// Pumps until the client has been quiet for `quiet`, or `limit` passes.
    pub fn settle(&mut self, quiet: Duration, limit: Duration) {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if !self.pump(quiet) {
                return;
            }
        }
    }

    pub fn screen(&self) -> Snapshot {
        self.terminal.snapshot()
    }

    /// Waits until `ready` holds for the screen, failing after `limit`.
    pub fn wait_for(&mut self, what: &str, limit: Duration, ready: impl Fn(&Snapshot) -> bool) {
        let deadline = Instant::now() + limit;
        loop {
            if ready(&self.screen()) {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {what}; screen:\n{}",
                    self.screen().render()
                );
            }
            self.pump(Duration::from_millis(20));
        }
    }

    pub fn wait_text(&mut self, text: &str) {
        let needle = text.to_string();
        self.wait_for(
            &format!("{text:?}"),
            Duration::from_secs(10),
            move |screen| screen.contains(&needle),
        );
    }

    pub fn type_bytes(&mut self, bytes: &[u8]) {
        self.input.write_all(bytes).unwrap();
        self.input.flush().unwrap();
    }

    pub fn type_str(&mut self, text: &str) {
        self.type_bytes(text.as_bytes());
    }

    /// Resizes the terminal, as dragging its window would.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        self.terminal.resize(rows as usize, cols as usize);
        self.rows = rows;
        self.cols = cols;
    }

    /// Stops or resumes reading the client's output.
    pub fn set_paused(&self, paused: bool) {
        *self.paused.lock().unwrap() = paused;
    }

    pub fn exited(&mut self) -> Option<u32> {
        self.child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| status.exit_code())
    }

    /// Waits for the client process to exit.
    pub fn wait_exit(&mut self, limit: Duration) -> u32 {
        let deadline = Instant::now() + limit;
        loop {
            self.pump(Duration::from_millis(10));
            if let Some(code) = self.exited() {
                self.settle(Duration::from_millis(50), Duration::from_secs(1));
                return code;
            }
            assert!(Instant::now() < deadline, "client did not exit");
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.process_id()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
