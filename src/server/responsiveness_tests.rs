use std::{
    env, fs, thread,
    time::{Duration, Instant},
};

use crate::protocol::{ClientMessage, MuxQuery, ServerMessage, read_message};

use super::{Event, tests::server_with_pending_bell};

#[test]
fn a_blocked_real_pty_does_not_stall_queries_or_shutdown() {
    let directory = env::temp_dir().join(format!(
        "mux-responsive-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let (mut server, events, mut client) = server_with_pending_bell(&directory);
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    server.clients.get_mut(&1).unwrap().session_id = Some(0);
    server.clients.get_mut(&1).unwrap().initialized = true;

    // Stop the real shell from reading its PTY. Once the kernel's PTY buffer
    // fills, the pane writer thread blocks while the server remains available.
    server.sessions[0].windows[0].panes[0]
        .writer
        .send(b"stty raw -echo; printf MUX_BLOCKED; sleep 60\r")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !server.sessions[0].windows[0].panes[0]
        .parser
        .screen()
        .contents()
        .contains("MUX_BLOCKED")
    {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "shell did not enter the blocking command"
        );
        let event = events.recv_timeout(remaining).unwrap();
        server.handle_event(event).unwrap();
    }

    let started = Instant::now();
    server
        .handle_event(Event::Client(
            1,
            ClientMessage::Paste("x".repeat(8 * 1024 * 1024)),
        ))
        .unwrap();
    server
        .handle_event(Event::Client(
            1,
            ClientMessage::Query {
                pane_id: None,
                query: MuxQuery::Sessions,
                json: false,
            },
        ))
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));

    let response: ServerMessage = read_message(&mut client).unwrap().unwrap();
    assert!(matches!(response, ServerMessage::Listing(lines) if lines.len() == 1));

    for pane in &mut server.sessions[0].windows[0].panes {
        pane.child.kill().unwrap();
    }
    assert!(
        server
            .handle_event(Event::Client(1, ClientMessage::Shutdown))
            .unwrap()
    );
    let response: ServerMessage = read_message(&mut client).unwrap().unwrap();
    assert!(matches!(response, ServerMessage::Detached));
    drop(server);
    fs::remove_dir_all(directory).unwrap();
}
