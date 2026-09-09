//! Live foreground processes, independent of the icons used to display them.

use std::path::PathBuf;

#[cfg(not(target_os = "macos"))]
use std::fs;

#[derive(Debug)]
pub(super) struct Process {
    pub pid: i32,
    pub parent: i32,
    pub group: i32,
    pub program: String,
}

#[cfg(not(target_os = "macos"))]
pub(super) fn process_cwd(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Read executable identities, never terminal titles or command arguments.
#[cfg(not(target_os = "macos"))]
pub(super) fn processes() -> Vec<Process> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
            let stat = fs::read_to_string(entry.path().join("stat")).ok()?;
            let (parent, group) = process_stat(&stat)?;
            let executable = fs::read_link(entry.path().join("exe")).ok()?;
            let program = executable.file_name()?.to_str()?.to_owned();
            Some(Process {
                pid,
                parent,
                group,
                program,
            })
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn process_stat(stat: &str) -> Option<(i32, i32)> {
    // comm can itself contain spaces and closing parentheses.
    let (_, fields) = stat.rsplit_once(')')?;
    let mut fields = fields.split_whitespace();
    match fields.next()? {
        "Z" | "X" | "x" | "T" | "t" => return None,
        _ => {}
    }
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
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
pub(super) fn processes() -> Vec<Process> {
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
            let parent = fields.next()?.parse().ok()?;
            let group = fields.next()?.parse().ok()?;
            let state = fields.next()?;
            if state.starts_with(['Z', 'T', 'X']) {
                return None;
            }
            // comm is an executable path; its spaces are part of the name.
            let path = fields.collect::<Vec<_>>().join(" ");
            let program = std::path::Path::new(&path)
                .file_name()?
                .to_str()?
                .to_owned();
            Some(Process {
                pid,
                parent,
                group,
                program,
            })
        })
        .collect()
}

/// Prefer the foreground job's root to its helpers. Node launchers are the one
/// exception: command-line tools installed through npm start as a Node wrapper
/// whose child is the actual executable. PID order breaks ties for pipelines
/// deterministically, without consulting the icon table.
pub(super) fn foreground_program(processes: &[Process], group: i32) -> Option<&str> {
    let candidates: Vec<_> = processes
        .iter()
        .filter(|process| process.group == group)
        .collect();
    candidates
        .iter()
        .copied()
        .min_by_key(|process| {
            let is_node_launcher = process.program == "node"
                && candidates
                    .iter()
                    .any(|child| child.parent == process.pid);
            let has_parent = candidates.iter().any(|parent| parent.pid == process.parent);
            (is_node_launcher, has_parent, process.pid != group, process.pid)
        })
        .map(|process| process.program.as_str())
}

/// Adding an icon only requires adding exact executable names here.
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

pub(super) fn program_icon(program: &str) -> &'static str {
    // Nix wraps executables as .<name>-wrapped. This is packaging syntax,
    // applied equally to every program rather than an icon-specific match.
    let program = program
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix("-wrapped"))
        .unwrap_or(program);
    PROCESS_ICONS
        .iter()
        .find(|(names, _)| names.contains(&program))
        .map_or(IDLE_ICON, |(_, icon)| *icon)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: i32, parent: i32, group: i32, program: &str) -> Process {
        Process {
            pid,
            parent,
            group,
            program: program.into(),
        }
    }

    #[test]
    fn icons_match_exact_executable_names() {
        for name in [
            "codex.txt",
            "my-codex",
            "python-tools",
            "notjj",
            "nvim codex",
            "claude.log",
        ] {
            assert_eq!(program_icon(name), IDLE_ICON, "{name}");
        }
        assert_eq!(program_icon("nvim"), "\u{e01f}\u{e020}\u{e021}");
        assert_eq!(program_icon(".nvim-wrapped"), program_icon("nvim"));
        assert_eq!(program_icon("python3.13"), "\u{e028}\u{e029}\u{e02a}");
        assert_eq!(program_icon("jj"), "");
        assert_eq!(program_icon("zsh"), "❯");
    }

    #[test]
    fn foreground_selection_ignores_icons_background_jobs_and_helpers() {
        let mut jobs = vec![
            process(10, 1, 10, "zsh"),
            process(20, 10, 20, "nvim"),
            process(21, 20, 20, "codex"),
            process(30, 10, 30, "claude"),
        ];
        assert_eq!(foreground_program(&jobs, 20), Some("nvim"));
        assert_eq!(foreground_program(&jobs, 10), Some("zsh"));
        jobs[1].program = "unknown-editor".into();
        assert_eq!(foreground_program(&jobs, 20), Some("unknown-editor"));
        jobs.reverse();
        assert_eq!(foreground_program(&jobs, 20), Some("unknown-editor"));
        assert_eq!(foreground_program(&jobs, 99), None);
    }

    #[test]
    fn foreground_selection_steps_past_a_node_command_launcher() {
        let jobs = vec![
            process(20, 10, 20, "node"),
            process(21, 20, 20, "codex"),
        ];
        assert_eq!(foreground_program(&jobs, 20), Some("codex"));
        assert_eq!(foreground_program(&jobs[0..1], 20), Some("node"));
    }

    #[test]
    fn a_pipeline_with_an_exited_leader_still_has_a_program() {
        let jobs = vec![process(22, 10, 20, "ssh"), process(21, 10, 20, "watch")];
        assert_eq!(foreground_program(&jobs, 20), Some("watch"));
        assert_eq!(foreground_program(&jobs[0..1], 20), Some("ssh"));
        assert_eq!(foreground_program(&[], 20), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn stat_parser_handles_parentheses_and_excludes_stopped_and_dead_jobs() {
        assert_eq!(
            process_stat("20 (a tricky ) name) S 10 20 10 0"),
            Some((10, 20))
        );
        for state in ["T", "t", "Z", "X", "x"] {
            assert_eq!(process_stat(&format!("20 (nvim) {state} 10 20 10 0")), None);
        }
        assert_eq!(process_stat("not a stat record"), None);
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
    fn live_identity_ignores_argv_and_disappears_after_exit() {
        use std::{os::unix::process::CommandExt, process::Command};
        let executable = std::env::current_exe().unwrap();
        let expected_program = executable.file_name().unwrap().to_str().unwrap();
        // Use our own executable: multicall utilities can exit when argv[0]
        // is changed. The fixture blocks on stdin while we inspect its identity.
        let mut child = Command::new(&executable)
            .arg0("codex")
            .args([
                "--ignored",
                "--exact",
                "server::process::tests::identity_child",
                "--skip",
                "claude",
                "--skip",
                "opencode",
                "--skip",
                "jj",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let snapshot = processes();
        child.kill().unwrap();
        child.wait().unwrap();
        let program = &snapshot
            .iter()
            .find(|process| process.pid == pid)
            .unwrap()
            .program;
        assert_eq!(program, expected_program);
        assert_eq!(program_icon(program), IDLE_ICON);
        assert!(!processes().iter().any(|process| process.pid == pid));
    }
}
