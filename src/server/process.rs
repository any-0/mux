//! What a pane is running, and the icon that says so.

use std::path::PathBuf;

#[cfg(not(target_os = "macos"))]
use std::fs;

#[cfg(not(target_os = "macos"))]
pub(super) fn process_cwd(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(target_os = "macos"))]
pub(super) fn process_info(pid: i32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/cmdline"))
        .ok()
        .map(|args| args.replace('\0', " ").trim().to_owned())
}

/// Every command running in one process group.
#[cfg(not(target_os = "macos"))]
pub(super) fn process_group_info(group: i32) -> Vec<String> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| process_group(*pid) == Some(group))
        .filter_map(process_info)
        .filter(|command| !command.is_empty())
        .collect()
}

/// The process group a process belongs to, from the fifth field of its stat
/// line. The name before it can hold spaces and brackets, so the fields are
/// counted from the closing bracket rather than from the start.
#[cfg(not(target_os = "macos"))]
fn process_group(pid: i32) -> Option<i32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after_name) = stat.rsplit_once(')')?;
    after_name.split_whitespace().nth(2)?.parse().ok()
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
pub(super) fn process_info(pid: i32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    (!output.stdout.is_empty()).then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Every command running in one process group.
#[cfg(target_os = "macos")]
pub(super) fn process_group_info(group: i32) -> Vec<String> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "command=", "-g", &group.to_string()])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The program a command line runs, without its arguments or its directory.
///
/// The Nix icon needs this rather than a substring test: every binary that came
/// from a store path has "nix" somewhere in its command line.
fn program_name(command: &str) -> &str {
    let program = command.split_whitespace().next().unwrap_or(command);
    program.rsplit('/').next().unwrap_or(program)
}

/// nix, its `nix-*` and `nixos-*` siblings, the nh wrapper, and direnv, which
/// spends its time loading a flake.
fn is_nix_command(program: &str) -> bool {
    matches!(program, "nix" | "nh" | "direnv")
        || program.starts_with("nix-")
        || program.starts_with("nixos-")
}

/// A test for what a pane is running, and the icon that matches it.
type ProcessIcon = (fn(&str) -> bool, &'static str);

/// What each process a window can be busy with looks like in the strip, most
/// specific first. A shell only wins when nothing it started matches, so a
/// window running a script shows what the script is doing.
const PROCESS_ICONS: &[ProcessIcon] = &[
    (
        |command| command.contains("codex"),
        "\u{e015}\u{e016}\u{e017}",
    ),
    (
        |command| command.contains("claude"),
        "\u{e012}\u{e013}\u{e014}",
    ),
    (
        |command| is_nix_command(program_name(command)),
        "\u{e019}\u{e01a}\u{e01b}",
    ),
    (
        |command| program_name(command) == "watch",
        "\u{e01c}\u{e01d}\u{e01e}",
    ),
    (
        |command| matches!(program_name(command), "nvim" | "vim"),
        "\u{e01f}\u{e020}\u{e021}",
    ),
    (
        |command| program_name(command) == "ssh",
        "\u{e022}\u{e023}\u{e024}",
    ),
    (
        |command| matches!(program_name(command), "cargo" | "rustc"),
        "\u{e025}\u{e026}\u{e027}",
    ),
    (
        |command| program_name(command).starts_with("python"),
        "\u{e028}\u{e029}\u{e02a}",
    ),
    (|command| command.ends_with("jj"), ""),
    (|command| program_name(command) == "bash", "$"),
    (|command| command.ends_with("zsh"), "❯"),
];

/// The window is idle, or mux cannot tell what it is running.
pub(super) const IDLE_ICON: &str = "·";

/// The icon for everything running in one foreground process group.
pub(super) fn process_group_icon(commands: &[String]) -> &'static str {
    PROCESS_ICONS
        .iter()
        .find(|(matches, _)| commands.iter().any(|command| matches(command)))
        .map_or(IDLE_ICON, |(_, icon)| *icon)
}
