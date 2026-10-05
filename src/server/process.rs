//! Live foreground processes, independent of the icons used to display them.

use std::path::PathBuf;

#[cfg(not(target_os = "macos"))]
use std::fs;

#[derive(Debug)]
pub(super) struct Process {
    pub pid: i32,
    pub group: i32,
    /// The name it was started as: `nvim`, `claude`, `python3`.
    pub name: String,
}

#[cfg(not(target_os = "macos"))]
pub(super) fn process_cwd(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Each process's name is the file name of `argv[0]`: what it was started as,
/// whatever the executable behind it is called (`claude` runs `claude.exe`,
/// Nix's `nvim` runs `.nvim-wrapped`). With `groups`, only processes in those
/// process groups are returned, skipping the rest before reading their names.
#[cfg(not(target_os = "macos"))]
pub(super) fn processes(groups: Option<&[i32]>) -> Vec<Process> {
    if groups.is_some_and(<[i32]>::is_empty) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
            let stat = fs::read_to_string(entry.path().join("stat")).ok()?;
            let group = process_group(&stat)?;
            if groups.is_some_and(|groups| !groups.contains(&group)) {
                return None;
            }
            let cmdline = fs::read(entry.path().join("cmdline")).ok()?;
            let argv0 = cmdline.split(|&byte| byte == 0).next()?;
            let name = std::str::from_utf8(argv0).ok()?.rsplit('/').next()?;
            (!name.is_empty()).then(|| Process {
                pid,
                group,
                name: name.to_owned(),
            })
        })
        .collect()
}

/// The process group of a live, running process; `None` for stopped and dead
/// ones, which are not what a pane is showing.
#[cfg(not(target_os = "macos"))]
fn process_group(stat: &str) -> Option<i32> {
    // comm can itself contain spaces and closing parentheses.
    let (_, fields) = stat.rsplit_once(')')?;
    let mut fields = fields.split_whitespace();
    if matches!(fields.next()?, "Z" | "X" | "x" | "T" | "t") {
        return None;
    }
    fields.nth(1)?.parse().ok()
}

#[cfg(target_os = "macos")]
pub(super) fn process_cwd(pid: u32) -> Option<PathBuf> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};

    let pid = i32::try_from(pid).ok()?;
    let mut info = unsafe { std::mem::zeroed::<nix::libc::proc_vnodepathinfo>() };
    let info_size = std::mem::size_of::<nix::libc::proc_vnodepathinfo>();
    let bytes_written = unsafe {
        nix::libc::proc_pidinfo(
            pid,
            nix::libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut nix::libc::proc_vnodepathinfo).cast(),
            info_size as i32,
        )
    };
    if bytes_written != info_size as i32 || info.pvi_cdir.vip_vi.vi_stat.vst_dev == 0 {
        return None;
    }

    let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    (!path.to_bytes().is_empty())
        .then(|| PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
}

#[cfg(target_os = "macos")]
pub(super) fn processes(groups: Option<&[i32]>) -> Vec<Process> {
    if groups.is_some_and(<[i32]>::is_empty) {
        return Vec::new();
    }
    let Ok(output) = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,pgid=,stat=,comm="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let _parent = fields.next()?;
            let group = fields.next()?.parse().ok()?;
            if groups.is_some_and(|groups| !groups.contains(&group)) {
                return None;
            }
            let state = fields.next()?;
            if state.starts_with(['Z', 'T', 'X']) {
                return None;
            }
            // comm is an executable path; its spaces are part of the name.
            let path = fields.collect::<Vec<_>>().join(" ");
            let name = std::path::Path::new(&path)
                .file_name()?
                .to_str()?
                .to_owned();
            Some(Process { pid, group, name })
        })
        .collect()
}

/// The icon for a foreground job: the first of its processes with one, the
/// job's leader first and then the others in the order they started. So
/// `claude` keeps its icon while it runs shell commands, and an npm `node`
/// launcher, `sudo` or the head of a pipeline gives way to the program after it.
pub(super) fn foreground_icon(processes: &[Process], group: i32) -> &'static str {
    let mut job: Vec<_> = processes
        .iter()
        .filter(|process| process.group == group)
        .collect();
    job.sort_by_key(|process| (process.pid != group, process.pid));
    job.iter()
        .map(|process| program_icon(&process.name))
        .find(|icon| *icon != IDLE_ICON)
        .unwrap_or(IDLE_ICON)
}

/// What each icon reads as for a font without mux's glyphs: three columns
/// of text, or one character that is centred in them.
const TEXT_ICONS: &[(&str, &str)] = &[
    ("\u{e02b}\u{e02c}\u{e02d}", "opn"),
    ("\u{e015}\u{e016}\u{e017}", "cdx"),
    ("\u{e012}\u{e013}\u{e014}", "cld"),
    ("\u{e019}\u{e01a}\u{e01b}", "nix"),
    ("\u{e01c}\u{e01d}\u{e01e}", "wch"),
    ("\u{e01f}\u{e020}\u{e021}", "vim"),
    ("\u{e022}\u{e023}\u{e024}", "ssh"),
    ("\u{e025}\u{e026}\u{e027}", "rs "),
    ("\u{e028}\u{e029}\u{e02a}", "py "),
    ("\u{e640}", "jj "),
    ("❯", "%"),
];

/// The text form of a program icon, for [`crate::config::Glyphs::Text`].
pub(super) fn text_icon(icon: &'static str) -> &'static str {
    TEXT_ICONS
        .iter()
        .find(|(font, _)| *font == icon)
        .map_or(icon, |(_, text)| *text)
}

/// Program names and their icons. Adding one is adding its name here.
const PROCESS_ICONS: &[(&[&str], &str)] = &[
    (&["opencode"], "\u{e02b}\u{e02c}\u{e02d}"),
    (&["codex"], "\u{e015}\u{e016}\u{e017}"),
    (&["claude"], "\u{e012}\u{e013}\u{e014}"),
    (
        &[
            "nix",
            "nix-build",
            "nix-shell",
            "nix-env",
            "nix-store",
            "nixos-rebuild",
            "nixos-install",
            "nh",
            "direnv",
        ],
        "\u{e019}\u{e01a}\u{e01b}",
    ),
    (&["watch"], "\u{e01c}\u{e01d}\u{e01e}"),
    (&["nvim", "vim"], "\u{e01f}\u{e020}\u{e021}"),
    (&["ssh"], "\u{e022}\u{e023}\u{e024}"),
    (&["cargo", "rustc"], "\u{e025}\u{e026}\u{e027}"),
    (
        &[
            "python",
            "python3",
            "python3.10",
            "python3.11",
            "python3.12",
            "python3.13",
            "python3.14",
        ],
        "\u{e028}\u{e029}\u{e02a}",
    ),
    (&["jj"], ""),
    (&["bash"], "$"),
    (&["zsh"], "❯"),
];

pub(super) const IDLE_ICON: &str = "·";

pub(super) fn program_icon(name: &str) -> &'static str {
    PROCESS_ICONS
        .iter()
        .find(|(names, _)| names.contains(&name))
        .map_or(IDLE_ICON, |(_, icon)| *icon)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: i32, group: i32, name: &str) -> Process {
        Process {
            pid,
            group,
            name: name.into(),
        }
    }

    #[test]
    fn icons_match_exact_program_names() {
        for name in [
            "codex.txt",
            "my-codex",
            "notjj",
            "claude.exe",
            ".nvim-wrapped",
        ] {
            assert_eq!(program_icon(name), IDLE_ICON, "{name}");
        }
        assert_eq!(program_icon("nvim"), "\u{e01f}\u{e020}\u{e021}");
        assert_eq!(program_icon("python3.13"), "\u{e028}\u{e029}\u{e02a}");
        assert_eq!(program_icon("zsh"), "❯");
    }

    #[test]
    fn a_job_shows_its_first_process_with_an_icon_leader_first() {
        let claude = program_icon("claude");
        let codex = program_icon("codex");
        let nvim = program_icon("nvim");
        // The leader wins over the commands it runs.
        let jobs = [process(30, 30, "claude"), process(31, 30, "bash")];
        assert_eq!(foreground_icon(&jobs, 30), claude);
        // A launcher without an icon gives way to what it started.
        let jobs = [process(20, 20, "node"), process(21, 20, "codex")];
        assert_eq!(foreground_icon(&jobs, 20), codex);
        // Followers in the order they started; other jobs never count.
        let jobs = [
            process(42, 40, "nvim"),
            process(41, 40, "cat"),
            process(43, 40, "ssh"),
            process(50, 50, "claude"),
        ];
        assert_eq!(foreground_icon(&jobs, 40), nvim);
        assert_eq!(foreground_icon(&jobs, 60), IDLE_ICON);
        assert_eq!(
            foreground_icon(&[process(70, 70, "mystery")], 70),
            IDLE_ICON
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn stat_parser_handles_parentheses_and_excludes_stopped_and_dead_jobs() {
        assert_eq!(process_group("20 (a tricky ) name) S 10 20 10 0"), Some(20));
        for state in ["T", "t", "Z", "X", "x"] {
            assert_eq!(
                process_group(&format!("20 (nvim) {state} 10 20 10 0")),
                None
            );
        }
        assert_eq!(process_group("not a stat record"), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    #[ignore = "subprocess fixture that waits for stdin to close"]
    fn identity_child() {
        use std::io::Read;
        std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_live_process_is_named_by_what_it_was_started_as() {
        use std::{os::unix::process::CommandExt, process::Command};
        // Our own executable, started as `codex`, blocks on stdin while we look.
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg0("/some/where/codex")
            .args([
                "--ignored",
                "--exact",
                "server::process::tests::identity_child",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let snapshot = processes(None);
        let found = snapshot.iter().find(|process| process.pid == pid).unwrap();
        assert_eq!(found.name, "codex");
        let filtered = processes(Some(&[found.group]));
        assert!(filtered.iter().all(|process| process.group == found.group));
        assert!(filtered.iter().any(|process| process.pid == pid));
        assert!(processes(Some(&[])).is_empty());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!processes(None).iter().any(|process| process.pid == pid));
    }
}
