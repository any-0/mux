//! What survives a restart: the session tree on disk, and the private
//! directories the daemon keeps it in.

use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use nix::{
    errno::Errno,
    fcntl::{Flock, FlockArg},
};
use portable_pty::CommandBuilder;
use serde::{Deserialize, Serialize};

use super::layout::{EVEN_SPLIT, PaneLayout, SplitAxis};

pub(super) const STATE_VERSION: u32 = 3;

/// State from before splits carried a ratio.
pub(super) const EVEN_SPLIT_STATE_VERSION: u32 = 2;

pub(super) const LEGACY_STATE_VERSION: u32 = 1;

const ZSHENV_WRAPPER: &str = r#"_mux_startup_zdotdir=$ZDOTDIR
ZDOTDIR=$MUX_ORIGINAL_ZDOTDIR
export ZDOTDIR
source "$ZDOTDIR/.zshenv"
ZDOTDIR=$_mux_startup_zdotdir
export ZDOTDIR
unset _mux_startup_zdotdir
"#;

pub(super) const ZSHRC_WRAPPER: &str = r#"ZDOTDIR=$MUX_ORIGINAL_ZDOTDIR
export ZDOTDIR
source "$ZDOTDIR/.zshrc"
autoload -Uz add-zsh-hook add-zle-hook-widget
_mux_prompt_start() { print -rn -- $'\e]777;mux-prompt-start\e\\' }
_mux_prompt_ready() { print -rn -- $'\e]777;mux-prompt-ready\e\\' }
add-zsh-hook precmd _mux_prompt_start
add-zle-hook-widget line-init _mux_prompt_ready
unset MUX_ORIGINAL_ZDOTDIR
"#;

#[derive(Deserialize, Serialize)]
pub(super) struct PersistedState {
    pub(super) version: u32,
    pub(super) next_session_id: usize,
    pub(super) next_pane_id: usize,
    pub(super) sessions: Vec<PersistedSession>,
    pub(super) last_active_pane: Option<usize>,
}

/// State written before splits could be resized.
///
/// bincode carries no field names, so an older file cannot be decoded into the
/// current types at all; the shapes it was written with are kept here and
/// converted on the way in.
#[derive(Deserialize, Serialize)]
pub(super) struct PersistedStateV2 {
    pub(super) version: u32,
    pub(super) next_session_id: usize,
    pub(super) next_pane_id: usize,
    pub(super) sessions: Vec<PersistedSessionV2>,
    pub(super) last_active_pane: Option<usize>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct PersistedStateV1 {
    pub(super) version: u32,
    pub(super) next_session_id: usize,
    pub(super) next_pane_id: usize,
    pub(super) sessions: Vec<PersistedSessionV2>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct PersistedSessionV2 {
    pub(super) id: usize,
    pub(super) name: String,
    pub(super) root: PathBuf,
    pub(super) windows: Vec<PersistedWindowV2>,
    pub(super) current_window: usize,
}

#[derive(Deserialize, Serialize)]
pub(super) struct PersistedWindowV2 {
    pub(super) panes: Vec<PersistedPane>,
    pub(super) layout: PaneLayoutV2,
    pub(super) active_pane: usize,
}

#[derive(Deserialize, Serialize)]
pub(super) enum PaneLayoutV2 {
    Pane(usize),
    Split {
        axis: SplitAxis,
        first: Box<PaneLayoutV2>,
        second: Box<PaneLayoutV2>,
    },
}

impl From<PaneLayoutV2> for PaneLayout {
    fn from(layout: PaneLayoutV2) -> Self {
        match layout {
            PaneLayoutV2::Pane(pane_id) => Self::Pane(pane_id),
            PaneLayoutV2::Split {
                axis,
                first,
                second,
            } => Self::Split {
                axis,
                ratio: EVEN_SPLIT,
                first: Box::new((*first).into()),
                second: Box::new((*second).into()),
            },
        }
    }
}

impl From<PersistedSessionV2> for PersistedSession {
    fn from(session: PersistedSessionV2) -> Self {
        Self {
            id: session.id,
            name: session.name,
            root: session.root,
            windows: session
                .windows
                .into_iter()
                .map(|window| PersistedWindow {
                    panes: window.panes,
                    layout: window.layout.into(),
                    active_pane: window.active_pane,
                    zoomed: false,
                    name: None,
                })
                .collect(),
            current_window: session.current_window,
        }
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct PersistedSession {
    pub(super) id: usize,
    pub(super) name: String,
    pub(super) root: PathBuf,
    pub(super) windows: Vec<PersistedWindow>,
    pub(super) current_window: usize,
}

#[derive(Deserialize, Serialize)]
pub(super) struct PersistedWindow {
    pub(super) panes: Vec<PersistedPane>,
    pub(super) layout: PaneLayout,
    pub(super) active_pane: usize,
    pub(super) zoomed: bool,
    pub(super) name: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct PersistedPane {
    pub(super) id: usize,
    pub(super) cwd: PathBuf,
    pub(super) rows: u16,
    pub(super) cols: u16,
}

pub(super) fn decode_persisted_state(bytes: &[u8]) -> Result<PersistedState> {
    let config = bincode::config::standard();
    let (version, _): (u32, usize) =
        bincode::serde::decode_from_slice(bytes, config).context("decode mux state version")?;
    match version {
        STATE_VERSION => {
            let (state, used): (PersistedState, usize) =
                bincode::serde::decode_from_slice(bytes, config).context("decode mux state")?;
            if used != bytes.len() {
                bail!("mux state contains trailing bytes");
            }
            Ok(state)
        }
        EVEN_SPLIT_STATE_VERSION => {
            let (legacy, used): (PersistedStateV2, usize) =
                bincode::serde::decode_from_slice(bytes, config)
                    .context("decode mux state saved before resizable splits")?;
            if used != bytes.len() {
                bail!("mux state contains trailing bytes");
            }
            Ok(PersistedState {
                version: STATE_VERSION,
                next_session_id: legacy.next_session_id,
                next_pane_id: legacy.next_pane_id,
                sessions: legacy.sessions.into_iter().map(Into::into).collect(),
                last_active_pane: legacy.last_active_pane,
            })
        }
        LEGACY_STATE_VERSION => {
            let (legacy, used): (PersistedStateV1, usize) =
                bincode::serde::decode_from_slice(bytes, config)
                    .context("decode legacy mux state")?;
            if used != bytes.len() {
                bail!("mux state contains trailing bytes");
            }
            Ok(PersistedState {
                version: STATE_VERSION,
                next_session_id: legacy.next_session_id,
                next_pane_id: legacy.next_pane_id,
                sessions: legacy.sessions.into_iter().map(Into::into).collect(),
                last_active_pane: None,
            })
        }
        version => bail!("unsupported mux state version {version}; expected {STATE_VERSION}"),
    }
}

pub(super) struct Persistence {
    pub(super) directory: PathBuf,
    pub(super) state_file: PathBuf,
}

pub(super) struct StateWriter {
    sender: Sender<StateCommand>,
    failures: Receiver<String>,
}

enum StateCommand {
    Save(PersistedState),
    Flush(PersistedState, SyncSender<Result<()>>),
}

impl Persistence {
    pub(super) fn open() -> Result<Self> {
        let state_home = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .context("neither XDG_STATE_HOME nor HOME is set")?;
        let directory = state_home.join("mux");
        fs::create_dir_all(&directory)
            .with_context(|| format!("create mux state directory {}", directory.display()))?;
        set_directory_permissions(&directory)?;
        Ok(Self {
            state_file: directory.join("state.bin"),
            directory,
        })
    }

    /// Claims this state directory for one daemon, or reports `None` when
    /// another daemon already holds it.
    ///
    /// The lock is released by the kernel when the daemon exits, so a crash
    /// never leaves the next one locked out.
    pub(super) fn lock(&self) -> Result<Option<Flock<File>>> {
        let path = self.directory.join("daemon.lock");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open daemon lock {}", path.display()))?;
        set_private_permissions(&path)?;
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => Ok(Some(lock)),
            Err((_, Errno::EWOULDBLOCK | Errno::EINTR)) => Ok(None),
            Err((_, errno)) => Err(errno).with_context(|| format!("lock {}", path.display())),
        }
    }

    pub(super) fn load(&self) -> Result<Option<PersistedState>> {
        let bytes = match fs::read(&self.state_file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("read mux state"),
        };
        decode_persisted_state(&bytes).map(Some)
    }

    pub(super) fn save(&self, state: &PersistedState) -> Result<()> {
        let bytes = bincode::serde::encode_to_vec(state, bincode::config::standard())
            .context("encode mux state")?;
        let temporary = self.directory.join("state.bin.tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&temporary)
                .context("open temporary mux state")?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        set_private_permissions(&temporary)?;
        fs::rename(&temporary, &self.state_file).context("commit mux state")?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    pub(super) fn state_writer(&self) -> StateWriter {
        let persistence = Self {
            directory: self.directory.clone(),
            state_file: self.state_file.clone(),
        };
        let (sender, receiver) = mpsc::channel();
        let (failure_sender, failures) = mpsc::channel();
        thread::spawn(move || state_writer(persistence, receiver, failure_sender));
        StateWriter { sender, failures }
    }

    fn pane_history_path(&self, pane_id: usize) -> PathBuf {
        self.directory.join(format!("pane-{pane_id}.ansi"))
    }

    pub(super) fn new_pane_history(&self, pane_id: usize) -> Result<File> {
        let path = self.pane_history_path(pane_id);
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("create pane history {}", path.display()))?;
        set_private_permissions(&path)?;
        drop(file);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("open pane history {}", path.display()))
    }

    /// Creates an unlinked file for scrollback blocks. The open handle keeps
    /// the bytes alive, while a crash or normal exit leaves no cache file to
    /// clean up or mistake for durable pane history.
    pub(super) fn new_scrollback_backing(&self) -> Result<File> {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        let number = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = self
            .directory
            .join(format!(".scrollback-{}-{number}", std::process::id()));
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("create scrollback backing {}", path.display()))?;
        set_private_permissions(&path)?;
        fs::remove_file(&path)
            .with_context(|| format!("unlink scrollback backing {}", path.display()))?;
        Ok(file)
    }

    pub(super) fn restored_pane_history(&self, pane_id: usize) -> Result<(File, u64)> {
        let path = self.pane_history_path(pane_id);
        let file =
            File::open(&path).with_context(|| format!("open pane history {}", path.display()))?;
        let length = file
            .metadata()
            .with_context(|| format!("inspect pane history {}", path.display()))?
            .len();
        Ok((file, length))
    }

    pub(super) fn resume_pane_history(&self, pane_id: usize, valid_length: u64) -> Result<File> {
        let path = self.pane_history_path(pane_id);
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .with_context(|| format!("open pane history {}", path.display()))?;
        file.set_len(valid_length)?;
        drop(file);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("resume pane history {}", path.display()))
    }

    /// Removes a closed pane's history. A journal that is already gone is the
    /// outcome this asks for, not a failure.
    pub(super) fn remove_pane_history(&self, pane_id: usize) -> Result<()> {
        let path = self.pane_history_path(pane_id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("remove pane history {}", path.display()))
            }
        }
    }
}

fn state_writer(
    persistence: Persistence,
    receiver: Receiver<StateCommand>,
    failures: Sender<String>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            StateCommand::Save(mut state) => loop {
                match receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(StateCommand::Save(newer)) => state = newer,
                    Ok(StateCommand::Flush(final_state, reply)) => {
                        let _ = reply.send(persistence.save(&final_state));
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Err(error) = persistence.save(&state) {
                            let _ = failures.send(format!("{error:#}"));
                        }
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        let _ = persistence.save(&state);
                        return;
                    }
                }
            },
            StateCommand::Flush(state, reply) => {
                let _ = reply.send(persistence.save(&state));
            }
        }
    }
}

impl StateWriter {
    pub(super) fn save(&self, state: PersistedState) -> Result<()> {
        self.sender
            .send(StateCommand::Save(state))
            .context("mux state writer stopped")
    }

    pub(super) fn flush(&self, state: PersistedState) -> Result<()> {
        let (sender, receiver) = mpsc::sync_channel(0);
        self.sender
            .send(StateCommand::Flush(state, sender))
            .context("mux state writer stopped")?;
        receiver.recv().context("mux state writer stopped")?
    }

    pub(super) fn poll_failure(&self) -> Option<anyhow::Error> {
        self.failures.try_recv().ok().map(anyhow::Error::msg)
    }
}

pub(super) struct ZshStartup {
    pub(super) directory: PathBuf,
    pub(super) original_zdotdir: OsString,
}

impl ZshStartup {
    /// Writes the startup files under the daemon's private state directory.
    ///
    /// Every new pane sources these, so they must live somewhere no other user
    /// can create or replace them.
    pub(super) fn create(state_directory: &Path) -> Result<Self> {
        let directory = state_directory.join("zsh-startup");
        private_directory(&directory)?;
        let zshenv = directory.join(".zshenv");
        let zshrc = directory.join(".zshrc");
        fs::write(&zshenv, ZSHENV_WRAPPER)?;
        fs::write(&zshrc, ZSHRC_WRAPPER)?;
        set_private_permissions(&zshenv)?;
        set_private_permissions(&zshrc)?;
        let original_zdotdir = std::env::var_os("ZDOTDIR")
            .or_else(|| std::env::var_os("HOME"))
            .context("neither ZDOTDIR nor HOME is set")?;
        Ok(Self {
            directory,
            original_zdotdir,
        })
    }

    pub(super) fn configure(&self, command: &mut CommandBuilder) {
        command.env("ZDOTDIR", &self.directory);
        command.env("MUX_ORIGINAL_ZDOTDIR", &self.original_zdotdir);
    }
}

impl Drop for ZshStartup {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.directory.join(".zshenv"));
        let _ = fs::remove_file(self.directory.join(".zshrc"));
        let _ = fs::remove_dir(&self.directory);
    }
}

#[cfg(unix)]
pub(super) fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Makes sure the socket lands in a directory only this user can reach.
///
/// A path set through `MUX` is the caller's own choice and is left alone; the
/// paths mux picks for itself must not be shared, because anyone who can write
/// beside the socket can also reach the Zsh startup files the daemon feeds to
/// every new pane.
pub(super) fn prepare_socket_directory(socket_path: &Path) -> Result<()> {
    if env::var_os("MUX").is_some() {
        return Ok(());
    }
    let Some(directory) = socket_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    else {
        return Ok(());
    };
    private_directory(directory)
}

/// Creates `path` unreachable by other users, or checks that an existing one is.
pub(super) fn private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    // symlink_metadata, so a symlink planted at this path is rejected instead of
    // silently followed somewhere else.
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .with_context(|| format!("create private directory {}", path.display()));
        }
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {}", path.display()));
        }
    };
    if !metadata.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    if metadata.uid() != nix::unistd::getuid().as_raw() {
        bail!("{} is owned by another user", path.display());
    }
    if metadata.mode() & 0o077 != 0 {
        bail!("{} is reachable by other users", path.display());
    }
    Ok(())
}
