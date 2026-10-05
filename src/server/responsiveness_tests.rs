use std::time::{Duration, Instant};

use super::tests::TestServer;
use crate::protocol::{ClientMessage, MuxQuery, ServerMessage, read_message};

#[test]
fn a_blocked_real_pty_does_not_stall_queries_or_shutdown() {
    let mut test = TestServer::new("responsive");
    test.attach();
    test.client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    // Once the shell stops reading and the PTY buffer fills, the pane's
    // writer thread blocks while the daemon must stay available.
    test.pane()
        .writer
        .send(b"stty raw -echo; printf MUX_BLOCKED; sleep 60\r")
        .unwrap();
    test.wait_output("MUX_BLOCKED");

    let started = Instant::now();
    test.send(1, ClientMessage::Paste("x".repeat(8 * 1024 * 1024)));
    let query = ClientMessage::Query {
        pane_id: None,
        query: MuxQuery::Sessions,
        json: false,
    };
    test.send(1, query);
    assert!(started.elapsed() < Duration::from_secs(1));
    let response = read_message(&mut test.client).unwrap();
    assert!(matches!(response, Some(ServerMessage::Listing(lines)) if lines.len() == 1));

    for pane in &mut test.server.sessions[0].windows[0].panes {
        pane.child.kill().unwrap();
    }
    let shutdown = super::Event::Client(1, ClientMessage::Shutdown);
    assert!(test.server.handle_event(shutdown).unwrap());
    let response = read_message(&mut test.client).unwrap();
    assert!(matches!(response, Some(ServerMessage::Detached)));
}
