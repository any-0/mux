mod client;
mod config;
mod frame;
mod protocol;
mod server;
mod vim;

use std::{env, ffi::OsString, path::PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::Theme;
use crate::protocol::{MuxCommand, MuxQuery};

fn main() {
    if let Err(error) = run() {
        eprintln!("mux: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    if let Some(query) = parse_query(&arguments)? {
        return client::query(query);
    }
    if let Some(command) = parse_command(&arguments)? {
        return client::command(command);
    }
    let mut arguments = arguments.into_iter();
    let mut config = None;
    let mut session = None;
    while let Some(argument) = arguments.next() {
        match argument.to_string_lossy().as_ref() {
            "__server" => {
                let socket = arguments
                    .next()
                    .map(PathBuf::from)
                    .ok_or_else(|| anyhow::anyhow!("missing server socket path"))?;
                if arguments.next().is_some() {
                    bail!("unexpected server argument")
                }
                return server::run(&socket);
            }
            "stop" | "kill-server" => {
                if arguments.next().is_some() {
                    bail!("kill-server takes no arguments")
                }
                return client::stop();
            }
            "--config" => {
                config = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--config needs a path"))?,
                ));
            }
            "--session" => {
                session = Some(
                    arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--session needs a name"))?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            unknown => bail!("unknown argument {unknown:?}; run mux --help"),
        }
    }
    client::attach(config.as_deref(), session)
}

/// Windows are addressed by their position in the bar, counting from one.
fn window_number(value: &OsString, command: &str) -> Result<u8> {
    let number = value
        .to_string_lossy()
        .parse::<u8>()
        .with_context(|| format!("{command} target must be a number"))?;
    if number == 0 {
        bail!("{command} target must be at least 1")
    }
    Ok(number)
}

/// Read-only commands, which print to stdout instead of changing anything.
fn parse_query(arguments: &[OsString]) -> Result<Option<MuxQuery>> {
    let Some(name) = arguments.first().map(|value| value.to_string_lossy()) else {
        return Ok(None);
    };
    let query = match name.as_ref() {
        "list-sessions" | "ls" => MuxQuery::Sessions,
        "list-windows" => MuxQuery::Windows,
        "list-panes" => MuxQuery::Panes,
        _ => return Ok(None),
    };
    if arguments.len() > 1 {
        bail!("{name} takes no arguments")
    }
    Ok(Some(query))
}

fn parse_command(arguments: &[OsString]) -> Result<Option<MuxCommand>> {
    let Some(name) = arguments.first().map(|value| value.to_string_lossy()) else {
        return Ok(None);
    };
    let rest = &arguments[1..];
    let no_arguments = |command: &str| {
        if rest.is_empty() {
            Ok(())
        } else {
            bail!("{command} takes no arguments")
        }
    };
    let command = match name.as_ref() {
        "choose-tree" => {
            no_arguments("choose-tree")?;
            MuxCommand::ChooseTree
        }
        "detach" | "detach-client" => {
            no_arguments("detach")?;
            MuxCommand::Detach
        }
        "new-window" => {
            no_arguments("new-window")?;
            MuxCommand::NewWindow
        }
        "new-session" => {
            let name = match rest {
                [] => None,
                [flag, name] if flag == "-s" => Some(name.to_string_lossy().into_owned()),
                _ => bail!("new-session accepts only -s NAME"),
            };
            MuxCommand::NewSession(name)
        }
        "set-session-root" => {
            no_arguments("set-session-root")?;
            MuxCommand::SetSessionRoot
        }
        "rename-session" => match rest {
            [name] => MuxCommand::RenameSession(name.to_string_lossy().into_owned()),
            _ => bail!("rename-session needs exactly one name"),
        },
        "rename-window" => match rest {
            // No name clears it, leaving the window to its program's title.
            [] => MuxCommand::RenameWindow(String::new()),
            [name] => MuxCommand::RenameWindow(name.to_string_lossy().into_owned()),
            _ => bail!("rename-window takes at most one name"),
        },
        "split-window" => match rest {
            [] => MuxCommand::SplitHorizontal,
            [flag] if flag == "-v" => MuxCommand::SplitHorizontal,
            [flag] if flag == "-h" => MuxCommand::SplitVertical,
            _ => bail!("split-window accepts only -h or -v"),
        },
        "select-pane" => match rest {
            [flag] if flag == "-L" => MuxCommand::FocusLeft,
            [flag] if flag == "-D" => MuxCommand::FocusDown,
            [flag] if flag == "-U" => MuxCommand::FocusUp,
            [flag] if flag == "-R" => MuxCommand::FocusRight,
            _ => bail!("select-pane needs one of -L, -D, -U, or -R"),
        },
        "resize-pane" if matches!(rest, [flag] if flag == "-Z") => MuxCommand::ZoomPane,
        "zoom-pane" => {
            no_arguments("zoom-pane")?;
            MuxCommand::ZoomPane
        }
        "resize-pane" => {
            let (flag, cells) = match rest {
                [flag] => (flag, 1),
                [flag, cells] => (
                    flag,
                    cells
                        .to_string_lossy()
                        .parse::<u16>()
                        .context("resize-pane cell count must be a number")?,
                ),
                _ => bail!("resize-pane needs -L, -D, -U, or -R and an optional cell count"),
            };
            match flag.to_string_lossy().as_ref() {
                "-L" => MuxCommand::ResizeLeft(cells),
                "-D" => MuxCommand::ResizeDown(cells),
                "-U" => MuxCommand::ResizeUp(cells),
                "-R" => MuxCommand::ResizeRight(cells),
                _ => bail!("resize-pane needs one of -L, -D, -U, or -R"),
            }
        }
        "break-pane" => {
            no_arguments("break-pane")?;
            MuxCommand::BreakPane
        }
        "join-pane" => {
            let (window, axis_is_vertical) = match rest {
                [flag, number] if flag == "-t" => (number, true),
                [orientation, flag, number] if flag == "-t" => (
                    number,
                    match orientation.to_string_lossy().as_ref() {
                        "-h" => true,
                        "-v" => false,
                        _ => bail!("join-pane accepts only -h or -v"),
                    },
                ),
                _ => bail!("join-pane needs -t NUMBER, optionally after -h or -v"),
            };
            MuxCommand::JoinPane {
                window: window_number(window, "join-pane")?,
                axis_is_vertical,
            }
        }
        "swap-window" => {
            let number = match rest {
                [number] => number,
                [flag, number] if flag == "-t" => number,
                _ => bail!("swap-window needs -t NUMBER"),
            };
            MuxCommand::SwapWindow(window_number(number, "swap-window")?)
        }
        "jump-to-bell" => {
            no_arguments("jump-to-bell")?;
            MuxCommand::JumpToBell
        }
        "kill-pane" => {
            no_arguments("kill-pane")?;
            MuxCommand::KillPane
        }
        "kill-session" => {
            no_arguments("kill-session")?;
            MuxCommand::KillSession
        }
        "select-window" => {
            let number = match rest {
                [number] => number,
                [flag, number] if flag == "-t" => number,
                _ => bail!("select-window needs -t NUMBER"),
            };
            let number = window_number(number, "select-window")?;
            if number > 9 {
                bail!("select-window target must be from 1 through 9")
            }
            MuxCommand::SelectWindow(number)
        }
        "vim-mode" => {
            no_arguments("vim-mode")?;
            MuxCommand::EnterVim
        }
        "set-theme" => match rest {
            [path] => MuxCommand::SetTheme(Theme::load(&PathBuf::from(path))?),
            _ => bail!("set-theme needs exactly one path"),
        },
        _ => return Ok(None),
    };
    Ok(Some(command))
}

fn print_help() {
    println!(
        "mux - a small personal terminal multiplexer\n\nUSAGE:\n    mux [--config PATH] [--session NAME]\n    mux COMMAND [ARGUMENTS]\n\nCOMMANDS:\n    kill-server                 Stop the daemon and its panes\n    list-sessions, ls           Print one line per session\n    list-windows                Print one line per window of the current session\n    list-panes                  Print one line per pane of the current window\n    choose-tree                 Open the session tree\n    detach                      Detach the active client\n    new-window                  Create a window\n    new-session [-s NAME]       Create and select a session\n    rename-session NAME         Rename the current session\n    rename-window [NAME]        Name the current window, or clear its name\n    split-window [-h|-v]        Split the active pane\n    select-pane -L|-D|-U|-R     Focus an adjacent pane\n    resize-pane -L|-D|-U|-R [N] Move the nearest divider by N cells\n    zoom-pane                   Toggle the active pane over the whole window\n    break-pane                  Move the active pane into a window of its own\n    join-pane [-h|-v] -t N      Move the active pane into window N\n    swap-window -t N            Exchange the current window with window N\n    select-window -t NUMBER     Select window 1 through 9\n    vim-mode                    Enter Vim mode\n    set-theme PATH              Apply colors to attached clients\n    kill-pane                   Kill the active pane\n    kill-session                Kill the current session\n    set-session-root            Use the active shell directory as session root\n    jump-to-bell                Jump to the first pending bell\n\nOPTIONS:\n    --config PATH    Apply user bindings after built-in defaults\n                     (default: $XDG_CONFIG_HOME/mux/config.toml)\n    --session NAME   Attach to or create a named session\n    -h, --help       Show this help"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_tmux_style_commands_for_existing_features() {
        assert_eq!(
            parse_command(&args(&["choose-tree"])).unwrap(),
            Some(MuxCommand::ChooseTree)
        );
        assert_eq!(
            parse_command(&args(&["split-window", "-h"])).unwrap(),
            Some(MuxCommand::SplitVertical)
        );
        assert_eq!(
            parse_command(&args(&["select-pane", "-L"])).unwrap(),
            Some(MuxCommand::FocusLeft)
        );
        assert_eq!(
            parse_command(&args(&["select-window", "-t", "3"])).unwrap(),
            Some(MuxCommand::SelectWindow(3))
        );
        assert_eq!(
            parse_command(&args(&["vim-mode"])).unwrap(),
            Some(MuxCommand::EnterVim)
        );
        assert_eq!(parse_command(&args(&["copy-mode"])).unwrap(), None);
    }

    #[test]
    fn attach_options_are_not_parsed_as_commands() {
        assert_eq!(parse_command(&args(&["--session", "work"])).unwrap(), None);
    }

    #[test]
    fn queries_are_separate_from_commands() {
        assert_eq!(
            parse_query(&args(&["list-sessions"])).unwrap(),
            Some(MuxQuery::Sessions)
        );
        assert_eq!(
            parse_query(&args(&["ls"])).unwrap(),
            Some(MuxQuery::Sessions)
        );
        assert_eq!(
            parse_query(&args(&["list-panes"])).unwrap(),
            Some(MuxQuery::Panes)
        );
        assert!(parse_query(&args(&["list-windows", "extra"])).is_err());
        assert_eq!(parse_query(&args(&["kill-pane"])).unwrap(), None);
    }

    #[test]
    fn panes_can_be_resized_zoomed_and_named() {
        assert_eq!(
            parse_command(&args(&["resize-pane", "-L"])).unwrap(),
            Some(MuxCommand::ResizeLeft(1))
        );
        assert_eq!(
            parse_command(&args(&["resize-pane", "-D", "5"])).unwrap(),
            Some(MuxCommand::ResizeDown(5))
        );
        // tmux spells zoom as a resize; both work.
        assert_eq!(
            parse_command(&args(&["resize-pane", "-Z"])).unwrap(),
            Some(MuxCommand::ZoomPane)
        );
        assert_eq!(
            parse_command(&args(&["zoom-pane"])).unwrap(),
            Some(MuxCommand::ZoomPane)
        );
        assert_eq!(
            parse_command(&args(&["rename-window", "logs"])).unwrap(),
            Some(MuxCommand::RenameWindow("logs".into()))
        );
        assert_eq!(
            parse_command(&args(&["rename-window"])).unwrap(),
            Some(MuxCommand::RenameWindow(String::new()))
        );
        assert!(parse_command(&args(&["resize-pane", "-X"])).is_err());
    }

    #[test]
    fn panes_and_windows_can_be_moved() {
        assert_eq!(
            parse_command(&args(&["break-pane"])).unwrap(),
            Some(MuxCommand::BreakPane)
        );
        assert_eq!(
            parse_command(&args(&["join-pane", "-t", "2"])).unwrap(),
            Some(MuxCommand::JoinPane {
                window: 2,
                axis_is_vertical: true,
            })
        );
        assert_eq!(
            parse_command(&args(&["join-pane", "-v", "-t", "3"])).unwrap(),
            Some(MuxCommand::JoinPane {
                window: 3,
                axis_is_vertical: false,
            })
        );
        assert_eq!(
            parse_command(&args(&["swap-window", "-t", "4"])).unwrap(),
            Some(MuxCommand::SwapWindow(4))
        );
        // Windows are counted from one, so zero is not a window.
        assert!(parse_command(&args(&["swap-window", "-t", "0"])).is_err());
        assert!(parse_command(&args(&["join-pane", "2"])).is_err());
    }
}
