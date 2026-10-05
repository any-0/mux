mod client;
mod config;
mod frame;
mod protocol;
mod server;
mod vim;

use std::{
    env,
    ffi::OsString,
    io::{self, BufRead, Write},
    path::PathBuf,
};

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
    let automatic = arguments.first().is_some_and(|argument| argument == "auto");
    let arguments = &arguments[usize::from(automatic)..];
    if !automatic {
        if let Some((query, pane_id, json)) = parse_query_invocation(arguments)? {
            return client::query(query, pane_id, json);
        }
        if let Some((command, pane_id)) = parse_command_invocation(arguments)? {
            return client::command(command, pane_id);
        }
    }
    let mut arguments = arguments.iter();
    let mut config = None;
    let mut session = None;
    while let Some(argument) = arguments.next() {
        match argument.to_string_lossy().as_ref() {
            "__server" => {
                let socket = arguments.next().context("missing server socket path")?;
                if arguments.next().is_some() {
                    bail!("unexpected server argument")
                }
                return server::run(&PathBuf::from(socket));
            }
            "stop" | "kill-server" => {
                if arguments.next().is_some() {
                    bail!("kill-server takes no arguments")
                }
                return client::stop();
            }
            "--config" => {
                config = Some(PathBuf::from(
                    arguments.next().context("--config needs a path")?,
                ));
            }
            "--session" => {
                let name = arguments.next().context("--session needs a name")?;
                session = Some(name.to_string_lossy().into_owned());
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            unknown => bail!("unknown argument {unknown:?}; run mux --help"),
        }
    }
    if automatic && client::over_ssh() {
        let stdin = io::stdin();
        let stdout = io::stdout();
        if !confirm_auto_attach(&mut stdin.lock(), &mut stdout.lock())? {
            return Ok(());
        }
    }
    client::attach(config.as_deref(), session)
}

fn confirm_auto_attach(reader: &mut impl BufRead, writer: &mut impl Write) -> Result<bool> {
    loop {
        write!(writer, "Attach to mux? [Y/n] ")?;
        writer.flush()?;

        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            writeln!(writer)?;
            return Ok(false);
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "" | "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => writeln!(writer, "Please answer y or n.")?,
        }
    }
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

/// `NUMBER` or `-t NUMBER`.
fn window_target(rest: &[OsString], command: &str) -> Result<u8> {
    match rest {
        [number] => window_number(number, command),
        [flag, number] if flag == "-t" => window_number(number, command),
        _ => bail!("{command} needs -t NUMBER"),
    }
}

/// Reads the ID following a `--pane` flag.
fn pane_option<'a>(
    pane_id: &mut Option<usize>,
    rest: &mut impl Iterator<Item = &'a OsString>,
) -> Result<()> {
    if pane_id.is_some() {
        bail!("--pane may be given only once")
    }
    let value = rest.next().context("--pane needs an ID")?;
    let id = value.to_string_lossy().parse();
    *pane_id = Some(id.context("--pane ID must be a number")?);
    Ok(())
}

/// Read-only commands, which print to stdout instead of changing anything.
fn parse_query_invocation(
    arguments: &[OsString],
) -> Result<Option<(MuxQuery, Option<usize>, bool)>> {
    let Some(name) = arguments.first().map(|value| value.to_string_lossy()) else {
        return Ok(None);
    };
    let query = match name.as_ref() {
        "list-sessions" | "ls" => MuxQuery::Sessions,
        "list-windows" => MuxQuery::Windows,
        "list-panes" => MuxQuery::Panes,
        _ => return Ok(None),
    };
    let mut pane_id = None;
    let mut json = false;
    let mut rest = arguments[1..].iter();
    while let Some(argument) = rest.next() {
        match argument.to_string_lossy().as_ref() {
            "--pane" => pane_option(&mut pane_id, &mut rest)?,
            "--json" if !json => json = true,
            "--json" => bail!("--json may be given only once"),
            unknown => bail!("{name} does not accept {unknown:?}"),
        }
    }
    Ok(Some((query, pane_id, json)))
}

fn parse_command_invocation(arguments: &[OsString]) -> Result<Option<(MuxCommand, Option<usize>)>> {
    let mut pane_id = None;
    let mut rest = arguments.iter();
    let mut command_arguments: Vec<_> = rest.next().cloned().into_iter().collect();
    while let Some(argument) = rest.next() {
        if argument == "--pane" {
            pane_option(&mut pane_id, &mut rest)?;
        } else {
            command_arguments.push(argument.clone());
        }
    }
    Ok(parse_command(&command_arguments)?.map(|command| (command, pane_id)))
}

fn parse_command(arguments: &[OsString]) -> Result<Option<MuxCommand>> {
    let Some(name) = arguments.first().map(|value| value.to_string_lossy()) else {
        return Ok(None);
    };
    let rest = &arguments[1..];
    let no_arguments = |command: &str, result: MuxCommand| {
        if rest.is_empty() {
            Ok(result)
        } else {
            bail!("{command} takes no arguments")
        }
    };
    let command = match name.as_ref() {
        "choose-tree" => no_arguments("choose-tree", MuxCommand::ChooseTree)?,
        "detach" | "detach-client" => no_arguments("detach", MuxCommand::Detach)?,
        "new-window" => no_arguments("new-window", MuxCommand::NewWindow)?,
        "new-session" => MuxCommand::NewSession(match rest {
            [] => None,
            [flag, name] if flag == "-s" => Some(name.to_string_lossy().into_owned()),
            _ => bail!("new-session accepts only -s NAME"),
        }),
        "set-session-root" => no_arguments("set-session-root", MuxCommand::SetSessionRoot)?,
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
        "focus-mode" => no_arguments("focus-mode", MuxCommand::ZoomPane)?,
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
        "break-pane" => no_arguments("break-pane", MuxCommand::BreakPane)?,
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
        "swap-window" => MuxCommand::SwapWindow(window_target(rest, "swap-window")?),
        "jump-to-bell" => no_arguments("jump-to-bell", MuxCommand::JumpToBell)?,
        "kill-pane" => no_arguments("kill-pane", MuxCommand::KillPane)?,
        "kill-session" => no_arguments("kill-session", MuxCommand::KillSession)?,
        "select-window" => {
            let number = window_target(rest, "select-window")?;
            if number > 9 {
                bail!("select-window target must be from 1 through 9")
            }
            MuxCommand::SelectWindow(number)
        }
        "vim-mode" => no_arguments("vim-mode", MuxCommand::EnterVim)?,
        "refresh-client" | "refresh" => no_arguments("refresh-client", MuxCommand::RefreshClient)?,
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
        "mux - a small personal terminal multiplexer

USAGE:
    mux [--config PATH] [--session NAME]
    mux auto [--config PATH] [--session NAME]
    mux COMMAND [ARGUMENTS] [--pane ID]

COMMANDS:
    auto                        Ask before attaching from an SSH login
    kill-server                 Stop the daemon and its panes
    list-sessions, ls [--json]  Print one line per session
    list-windows [--json]       Print one line per window of the current session
    list-panes [--json]         Print one line per pane of the current window
    choose-tree                 Open the session tree
    detach                      Detach the active client
    new-window                  Create a window
    new-session [-s NAME]       Create and select a session
    rename-session NAME         Rename the current session
    rename-window [NAME]        Name the current window, or clear its name
    split-window [-h|-v]        Split the active pane
    select-pane -L|-D|-U|-R     Focus an adjacent pane
    resize-pane -L|-D|-U|-R [N] Move the nearest divider by N cells
    focus-mode                  Toggle focus mode for the active pane
    break-pane                  Move the active pane into a window of its own
    join-pane [-h|-v] -t N      Move the active pane into window N
    swap-window -t N            Exchange the current window with window N
    select-window -t NUMBER     Select window 1 through 9
    vim-mode                    Enter Vim mode
    set-theme PATH              Apply colors to attached clients
    kill-pane                   Kill the active pane
    kill-session                Kill the current session
    set-session-root            Use the active shell directory as session root
    jump-to-bell                Jump to the first pending bell

OPTIONS:
    --config PATH    Apply user bindings after built-in defaults
                     (default: $XDG_CONFIG_HOME/mux/config.toml)
    --session NAME   Attach to or create a named session
    --pane ID        Target a command or query at a stable pane ID
    --json           Print a list query as structured JSON
    -h, --help       Show this help"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_tmux_style_commands() {
        use MuxCommand::*;
        for (input, expected) in [
            (&["choose-tree"][..], ChooseTree),
            (&["split-window", "-h"], SplitVertical),
            (&["select-pane", "-L"], FocusLeft),
            (&["select-window", "-t", "3"], SelectWindow(3)),
            (&["vim-mode"], EnterVim),
            (&["resize-pane", "-L"], ResizeLeft(1)),
            (&["resize-pane", "-D", "5"], ResizeDown(5)),
            // tmux spells focus mode as a resize; both work.
            (&["resize-pane", "-Z"], ZoomPane),
            (&["focus-mode"], ZoomPane),
            (&["rename-window", "logs"], RenameWindow("logs".into())),
            (&["rename-window"], RenameWindow(String::new())),
            (&["break-pane"], BreakPane),
            (
                &["join-pane", "-t", "2"],
                JoinPane {
                    window: 2,
                    axis_is_vertical: true,
                },
            ),
            (
                &["join-pane", "-v", "-t", "3"],
                JoinPane {
                    window: 3,
                    axis_is_vertical: false,
                },
            ),
            (&["swap-window", "-t", "4"], SwapWindow(4)),
        ] {
            assert_eq!(parse_command(&args(input)).unwrap(), Some(expected));
        }
        // Left for attach parsing.
        assert_eq!(parse_command(&args(&["copy-mode"])).unwrap(), None);
        assert_eq!(parse_command(&args(&["--session", "work"])).unwrap(), None);
        for invalid in [
            &["resize-pane", "-X"][..],
            // Windows are counted from one, so zero is not a window.
            &["swap-window", "-t", "0"],
            &["join-pane", "2"],
            &["select-window", "10"],
            &["kill-pane", "extra"],
        ] {
            assert!(parse_command(&args(invalid)).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn auto_attach_defaults_to_yes_reprompts_and_declines_eof() {
        for (input, expected, prompt) in [
            ("\n", true, "Attach to mux? [Y/n] "),
            ("no\n", false, "Attach to mux? [Y/n] "),
            (
                "maybe\ny\n",
                true,
                "Attach to mux? [Y/n] Please answer y or n.\nAttach to mux? [Y/n] ",
            ),
            ("", false, "Attach to mux? [Y/n] \n"),
        ] {
            let mut output = Vec::new();
            assert_eq!(
                confirm_auto_attach(&mut input.as_bytes(), &mut output).unwrap(),
                expected
            );
            assert_eq!(String::from_utf8(output).unwrap(), prompt);
        }
    }

    #[test]
    fn parses_queries_and_their_options() {
        assert_eq!(
            parse_query_invocation(&args(&["ls"])).unwrap(),
            Some((MuxQuery::Sessions, None, false))
        );
        assert_eq!(
            parse_query_invocation(&args(&["list-panes", "--pane", "42", "--json"])).unwrap(),
            Some((MuxQuery::Panes, Some(42), true))
        );
        assert_eq!(parse_query_invocation(&args(&["kill-pane"])).unwrap(), None);
        assert!(parse_query_invocation(&args(&["list-windows", "extra"])).is_err());
        assert!(parse_query_invocation(&args(&["ls", "--pane"])).is_err());
        assert!(parse_query_invocation(&args(&["ls", "--json", "--json"])).is_err());
    }

    #[test]
    fn commands_accept_a_pane_option() {
        assert_eq!(
            parse_command_invocation(&args(&["resize-pane", "-D", "5", "--pane", "17"])).unwrap(),
            Some((MuxCommand::ResizeDown(5), Some(17)))
        );
        assert!(parse_command_invocation(&args(&["kill-pane", "--pane", "gone"])).is_err());
        assert!(
            parse_command_invocation(&args(&["kill-pane", "--pane", "1", "--pane", "2"])).is_err()
        );
    }
}
