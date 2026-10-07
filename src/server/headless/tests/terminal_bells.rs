//! Terminal-bell companion facts: boot-scoped latest serials plus cumulative
//! per-connection focus frontiers, exposed as an optional endpoint control.

use super::*;

/// The named optional control carrying terminal-bell facts.
const TERMINAL_BELLS_KIND: &str = "endpoint.terminal-bells.v1";

/// Decode one framed control message.
fn decode_control(bytes: Vec<u8>) -> Option<(String, serde_json::Value)> {
    match read_server_message(bytes) {
        ServerMessage::EndpointControl { kind, data } => Some((
            kind,
            serde_json::from_str(&data).expect("decode endpoint control payload"),
        )),
        ServerMessage::TerminalBell { .. }
        | ServerMessage::ClientShellEndpointResponseChunk { .. } => None,
        other => panic!("unexpected control message {other:?}"),
    }
}

/// Drain already-queued controls until one terminal-bells control arrives.
fn recv_terminal_bells(control: &std::sync::mpsc::Receiver<Vec<u8>>) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let bytes = control.recv_timeout(remaining).unwrap_or_else(|error| {
            panic!("waiting for a terminal-bells control failed: {error}");
        });
        if let Some((kind, payload)) = decode_control(bytes) {
            if kind == TERMINAL_BELLS_KIND {
                return payload;
            }
        }
    }
    panic!("timed out scanning for a terminal-bells control");
}

/// Drain queued controls for a short window and report whether any
/// terminal-bells control is still pending. Non-bell controls are consumed
/// without failing the check.
fn terminal_bells_pending(control: &std::sync::mpsc::Receiver<Vec<u8>>) -> bool {
    while let Ok(bytes) = control.recv_timeout(Duration::from_millis(200)) {
        if let Some((kind, _)) = decode_control(bytes) {
            if kind == TERMINAL_BELLS_KIND {
                return true;
            }
        }
    }
    false
}

/// Drain until the original raw BEL forwarding arrives and check its count.
/// The audible BEL and the companion facts share the control lane; the raw
/// message must keep flowing unchanged next to the new control.
fn recv_raw_terminal_bell(control: &std::sync::mpsc::Receiver<Vec<u8>>, expected_count: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Ok(bytes) = control.recv_timeout(remaining) else {
            panic!("timed out waiting for the raw terminal bell");
        };
        if let ServerMessage::TerminalBell { count } = read_server_message(bytes) {
            assert_eq!(count, expected_count, "raw BEL count must be unchanged");
            return;
        }
    }
    panic!("timed out scanning for the raw terminal bell");
}

/// Wait for the endpoint response, preserving companions that may legally
/// arrive before the response (including BELs drained before navigation).
fn recv_endpoint_response(
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
    request_id: &str,
) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut bells = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let bytes = control.recv_timeout(remaining).unwrap_or_else(|error| {
            panic!("waiting for endpoint response {request_id} failed: {error}");
        });
        match read_server_message(bytes) {
            ServerMessage::ClientShellEndpointResponseChunk {
                request_id: seen,
                final_chunk,
                ..
            } => {
                assert_eq!(seen, request_id, "unexpected endpoint response chunk");
                assert!(final_chunk);
                return bells;
            }
            ServerMessage::EndpointControl { kind, data } if kind == TERMINAL_BELLS_KIND => {
                bells.push(serde_json::from_str(&data).expect("bell companion"));
            }
            ServerMessage::EndpointControl { .. } | ServerMessage::TerminalBell { .. } => {}
            other => panic!("unexpected control message {other:?}"),
        }
    }
    panic!("timed out scanning for endpoint response {request_id}");
}

fn bell_test_server() -> (HeadlessServer, String, String) {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("bells");
    let second_tab = workspace.test_add_tab(Some("build"));
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let tab_ids = server
        .app
        .session_snapshot()
        .tabs
        .into_iter()
        .map(|tab| tab.tab_id)
        .collect::<Vec<_>>();
    let first_tab_id = tab_ids[0].clone();
    let second_tab_id = tab_ids[second_tab].clone();
    (server, first_tab_id, second_tab_id)
}

fn dispatch_bell(server: &mut HeadlessServer, pane_id: crate::layout::PaneId) {
    server.handle_internal_event_with_forwarding(AppEvent::TerminalBell { pane_id, count: 1 });
}

fn pane_id_of_tab(server: &HeadlessServer, tab_id: &str) -> crate::layout::PaneId {
    let (ws_idx, tab_idx) = server.app.parse_tab_id(tab_id).expect("parse tab id");
    server.app.state.workspaces[ws_idx].tabs[tab_idx].root_pane
}

/// Send one endpoint TabFocus request and wait for its response delivery.
/// TabFocus spawns the response waiter, so a response-ready server event and
/// a response chunk are guaranteed to follow.
async fn focus_tab_via_endpoint(
    server: &mut HeadlessServer,
    client_id: u64,
    request_id: &str,
    tab_id: &str,
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Vec<serde_json::Value> {
    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id,
            boot_id: server.client_shell_boot_id.clone(),
            request: Box::new(api::schema::Request {
                id: request_id.into(),
                method: api::schema::Method::TabFocus(api::schema::TabTarget {
                    tab_id: tab_id.to_owned(),
                }),
            }),
        })
    );
    let response_ready = server
        .server_event_rx
        .recv()
        .await
        .expect("focus response ready");
    server.handle_server_event(response_ready);
    recv_endpoint_response(control, request_id)
}

/// Assert the payload's latest/frontier entry, treating absent map entries as
/// zero, matching the wire contract where only nonzero facts are serialized.
fn assert_map_value(payload: &serde_json::Value, map: &str, tab_id: &str, expected: u64) {
    let actual = payload[map]
        .as_object()
        .and_then(|entries| entries.get(tab_id))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert_eq!(actual, expected, "{map}[{tab_id}]");
}

#[tokio::test]
async fn bell_alone_publishes_companion_without_rendering() {
    let (mut server, _first_tab_id, second_tab_id) = bell_test_server();
    let (control, render) = connect_test_shell(&mut server, 7, 80, 24);
    let _ = recv_terminal_bells(&control);
    while render.try_recv().is_ok() {}

    // Exercise the ordinary event drain. No render, focus, or rename may be
    // needed to publish a background BEL; hidden panes need no surface work.
    let pane_id = pane_id_of_tab(&server, &second_tab_id);
    server
        .app
        .event_tx
        .try_send(AppEvent::TerminalBell { pane_id, count: 1 })
        .unwrap();
    assert!(!server.drain_internal_events_with_forwarding());
    recv_raw_terminal_bell(&control, 1);
    let bells = recv_terminal_bells(&control);
    assert_map_value(&bells, "latest_by_tab", &second_tab_id, 1);
    assert_map_value(&bells, "focused_through_by_tab", &second_tab_id, 0);
    assert!(
        render.try_recv().is_err(),
        "metadata must not force pane rendering"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn connected_shell_receives_terminal_bells_seed() {
    let (mut server, first_tab_id, second_tab_id) = bell_test_server();
    let (control, _render) = connect_test_shell(&mut server, 7, 80, 24);

    let bells = recv_terminal_bells(&control);
    assert_eq!(bells["boot_id"], server.client_shell_boot_id.as_str());
    // Complete live-tab domain: both tabs present, zero means no bell yet.
    assert_map_value(&bells, "latest_by_tab", &first_tab_id, 0);
    assert_map_value(&bells, "latest_by_tab", &second_tab_id, 0);

    // A bell in the tab this client is viewing advances both latest and its
    // frontier, so no marker is owed. The original raw BEL forwarding must
    // keep flowing unchanged next to the new companion control.
    let pane = pane_id_of_tab(&server, &first_tab_id);
    dispatch_bell(&mut server, pane);
    recv_raw_terminal_bell(&control, 1);
    // A bell in the other tab advances only latest.
    let pane = pane_id_of_tab(&server, &second_tab_id);
    dispatch_bell(&mut server, pane);
    recv_raw_terminal_bell(&control, 1);
    server.render_and_stream();

    let bells = recv_terminal_bells(&control);
    assert_map_value(&bells, "latest_by_tab", &first_tab_id, 1);
    assert_map_value(&bells, "latest_by_tab", &second_tab_id, 2);
    assert_map_value(&bells, "focused_through_by_tab", &first_tab_id, 1);
    assert_map_value(&bells, "focused_through_by_tab", &second_tab_id, 0);

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn background_tab_bell_survives_focus_moves_and_marks_only_unseen() {
    let (mut server, first_tab_id, second_tab_id) = bell_test_server();
    let (control, _render) = connect_test_shell(&mut server, 7, 80, 24);
    let _ = recv_terminal_bells(&control);

    // BEL in the background tab, then focus it: the frontier must cover it.
    let pane = pane_id_of_tab(&server, &second_tab_id);
    dispatch_bell(&mut server, pane);
    focus_tab_via_endpoint(&mut server, 7, "focus-second", &second_tab_id, &control).await;
    server.render_and_stream();

    let bells = recv_terminal_bells(&control);
    assert_map_value(&bells, "latest_by_tab", &second_tab_id, 1);
    assert_map_value(&bells, "focused_through_by_tab", &second_tab_id, 1);

    // Reverse order: leave the tab first, then a bell arrives. The frontier
    // must stay below latest so the marker survives.
    focus_tab_via_endpoint(&mut server, 7, "focus-first", &first_tab_id, &control).await;
    let pane = pane_id_of_tab(&server, &second_tab_id);
    dispatch_bell(&mut server, pane);
    server.render_and_stream();

    let bells = recv_terminal_bells(&control);
    assert_map_value(&bells, "latest_by_tab", &second_tab_id, 2);
    assert_map_value(&bells, "focused_through_by_tab", &second_tab_id, 1);

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn queued_bell_before_navigation_is_attributed_to_previous_focus() {
    let (mut server, first_tab_id, second_tab_id) = bell_test_server();
    let (control, _render) = connect_test_shell(&mut server, 7, 80, 24);
    let _ = recv_terminal_bells(&control);

    // Queue a BEL while the client still views the first tab, then navigate
    // away in the same handler boundary. The drain before navigation must
    // attribute the bell to the previous committed focus.
    let bell_pane = pane_id_of_tab(&server, &first_tab_id);
    server
        .app
        .event_tx
        .try_send(AppEvent::TerminalBell {
            pane_id: bell_pane,
            count: 1,
        })
        .expect("queue bell");
    let observed =
        focus_tab_via_endpoint(&mut server, 7, "focus-second", &second_tab_id, &control).await;
    let bells = observed
        .into_iter()
        .last()
        .unwrap_or_else(|| recv_terminal_bells(&control));
    assert_map_value(&bells, "latest_by_tab", &first_tab_id, 1);
    assert_map_value(&bells, "focused_through_by_tab", &first_tab_id, 1);

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn inactive_reconnect_seed_preserves_client_watermarks_until_selection() {
    use crate::client::endpoint::ClientEndpointId;
    use crate::client::{ClientShellConfig, ClientShellState};

    fn receive(client: &mut ClientShellState, generation: u64, value: serde_json::Value) {
        client.receive_endpoint_terminal_bells(
            &ClientEndpointId::Local,
            Some(generation),
            serde_json::from_value(value).unwrap(),
        );
    }

    let (mut server, first_tab_id, second_tab_id) = bell_test_server();
    let (control, _render) = connect_test_shell(&mut server, 7, 80, 24);
    let initial_seed = recv_terminal_bells(&control);
    let mut client = ClientShellState::new(ClientShellConfig::from_config(
        &crate::config::Config::default(),
    ));
    client.set_snapshot(client_shell_snapshot(&control));
    receive(&mut client, 1, initial_seed);
    let first_pane = pane_id_of_tab(&server, &first_tab_id);
    dispatch_bell(&mut server, first_pane);
    receive(&mut client, 1, recv_terminal_bells(&control));
    let second_pane = pane_id_of_tab(&server, &second_tab_id);
    dispatch_bell(&mut server, second_pane);
    receive(&mut client, 1, recv_terminal_bells(&control));
    assert!(client.test_tab_owes_terminal_bell(&ClientEndpointId::Local, &second_tab_id));
    assert!(!client.test_tab_owes_terminal_bell(&ClientEndpointId::Local, &first_tab_id));

    server.remove_client_and_resize_if_needed(7);
    // Another viewer can change the server default while this endpoint is
    // disconnected. Reconnecting it in the background is not a visit.
    server.app.state.workspaces[0].active_tab = 1;
    let (writer, reconnected_control, _reconnected_render) = test_client_writer();
    server.handle_server_event(ServerEvent::ClientShellConnected {
        surface_reuse: false,
        surface_delta: false,
        surface_scroll: false,
        client_id: 8,
        surface_cols: 80,
        surface_rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        direct_graphics: false,
        endpoint_keybindings: false,
        mouse_capture: false,
        surface_active: false,
        writer,
    });
    // Consume the actual reliable producer seed, not a handcrafted frontier.
    receive(&mut client, 2, recv_terminal_bells(&reconnected_control));
    assert!(
        client.test_tab_owes_terminal_bell(&ClientEndpointId::Local, &second_tab_id),
        "inactive reconnect must retain unread bells"
    );
    assert!(!client.test_tab_owes_terminal_bell(&ClientEndpointId::Local, &first_tab_id));
    let _ = client_shell_snapshot(&reconnected_control);

    server.set_client_shell_surface_active(8, true).unwrap();
    server.publish_terminal_bells_companion(8);
    receive(&mut client, 2, recv_terminal_bells(&reconnected_control));
    assert!(
        !client.test_tab_owes_terminal_bell(&ClientEndpointId::Local, &second_tab_id),
        "actual endpoint selection clears its selected tab"
    );
    assert!(
        !client.test_tab_owes_terminal_bell(&ClientEndpointId::Local, &first_tab_id),
        "prior acknowledgements survive the reconnect"
    );
    shutdown_test_runtimes(&mut server);
}

#[cfg(unix)]
#[tokio::test]
async fn queued_navigation_accepts_response_before_bell_companion() {
    let (mut server, first_tab_id, second_tab_id) = bell_test_server();
    let (control, _render) = connect_test_shell(&mut server, 7, 80, 24);
    let _ = recv_terminal_bells(&control);
    server.clients.get_mut(&7).unwrap().writer = Some(ClientWriter::test_paused());
    let pane_id = pane_id_of_tab(&server, &first_tab_id);
    server
        .app
        .event_tx
        .try_send(AppEvent::TerminalBell { pane_id, count: 1 })
        .unwrap();
    server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
        client_id: 7,
        boot_id: server.client_shell_boot_id.clone(),
        request: Box::new(api::schema::Request {
            id: "paused-focus".into(),
            method: api::schema::Method::TabFocus(api::schema::TabTarget {
                tab_id: second_tab_id,
            }),
        }),
    });
    let response = server.server_event_rx.recv().await.unwrap();
    server.handle_server_event(response);
    let frames = server.clients[&7].writer.as_ref().unwrap().test_drain();
    let (tx, rx) = std::sync::mpsc::channel();
    for frame in frames {
        tx.send(frame).unwrap();
    }
    let observed = recv_endpoint_response(&rx, "paused-focus");
    assert!(
        observed.is_empty(),
        "paused writer drains reliable reply first"
    );
    let bells = observed
        .into_iter()
        .last()
        .unwrap_or_else(|| recv_terminal_bells(&rx));
    assert_map_value(&bells, "latest_by_tab", &first_tab_id, 1);
    assert_map_value(&bells, "focused_through_by_tab", &first_tab_id, 1);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn two_clients_acknowledge_independently() {
    let (mut server, first_tab_id, second_tab_id) = bell_test_server();
    let (first_control, _first_render) = connect_test_shell(&mut server, 7, 80, 24);
    let (second_control, _second_render) = connect_test_shell(&mut server, 8, 80, 24);
    let _ = recv_terminal_bells(&first_control);
    let _ = recv_terminal_bells(&second_control);

    // Both clients start on the first tab; one moves to the second tab.
    focus_tab_via_endpoint(
        &mut server,
        8,
        "focus-second",
        &second_tab_id,
        &second_control,
    )
    .await;

    // A bell in the first tab: client 8 views the second tab, so its frontier
    // for the first tab must not advance, while client 7's does.
    let pane = pane_id_of_tab(&server, &first_tab_id);
    dispatch_bell(&mut server, pane);
    server.render_and_stream();

    let first = recv_terminal_bells(&first_control);
    assert_map_value(&first, "latest_by_tab", &first_tab_id, 1);
    assert_map_value(&first, "focused_through_by_tab", &first_tab_id, 1);

    let second = recv_terminal_bells(&second_control);
    assert_map_value(&second, "latest_by_tab", &first_tab_id, 1);
    assert_map_value(&second, "focused_through_by_tab", &first_tab_id, 0);

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn surface_inactive_client_does_not_acknowledge_current_tab_bells() {
    let (mut server, first_tab_id, _second_tab_id) = bell_test_server();
    let (control, _render) = connect_test_shell(&mut server, 7, 80, 24);
    let _ = recv_terminal_bells(&control);

    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id: 7,
            boot_id: server.client_shell_boot_id.clone(),
            request: Box::new(api::schema::Request {
                id: "surface-off".into(),
                method: api::schema::Method::ClientShellSurfaceSet(
                    api::schema::ClientShellSurfaceSetParams { active: false },
                ),
            }),
        })
    );
    // ClientShellSurfaceSet is answered inline without a response waiter, so
    // no response-ready server event follows; drain the queued reply.
    recv_endpoint_response(&control, "surface-off");

    let pane = pane_id_of_tab(&server, &first_tab_id);
    dispatch_bell(&mut server, pane);
    server.render_and_stream();

    let bells = recv_terminal_bells(&control);
    assert_map_value(&bells, "latest_by_tab", &first_tab_id, 1);
    assert_map_value(&bells, "focused_through_by_tab", &first_tab_id, 0);

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn queued_bell_before_surface_deactivation_is_acknowledged() {
    let (mut server, first_tab_id, _) = bell_test_server();
    let (control, _render) = connect_test_shell(&mut server, 7, 80, 24);
    let _ = recv_terminal_bells(&control);
    let pane_id = pane_id_of_tab(&server, &first_tab_id);
    server
        .app
        .event_tx
        .try_send(AppEvent::TerminalBell { pane_id, count: 1 })
        .unwrap();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
            client_id: 7,
            boot_id: server.client_shell_boot_id.clone(),
            request: Box::new(api::schema::Request {
                id: "deactivate-after-bell".into(),
                method: api::schema::Method::ClientShellSurfaceSet(
                    api::schema::ClientShellSurfaceSetParams { active: false },
                ),
            }),
        })
    );
    let bells = recv_terminal_bells(&control);
    assert_map_value(&bells, "latest_by_tab", &first_tab_id, 1);
    assert_map_value(&bells, "focused_through_by_tab", &first_tab_id, 1);
    assert!(!server.clients[&7].shell_surface_active);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn topology_fallback_acknowledges_only_the_viewing_client() {
    let (mut server, _first_tab_id, second_tab_id) = bell_test_server();
    let third_index = server.app.state.workspaces[0].test_add_tab(Some("third"));
    server.app.state.ensure_test_terminals();
    let third_tab_id = server.app.public_tab_id(0, third_index).unwrap();
    let (first_control, _first_render) = connect_test_shell(&mut server, 7, 80, 24);
    let (second_control, _second_render) = connect_test_shell(&mut server, 8, 80, 24);
    let _ = recv_terminal_bells(&first_control);
    let _ = recv_terminal_bells(&second_control);
    focus_tab_via_endpoint(
        &mut server,
        8,
        "other-client-third",
        &third_tab_id,
        &second_control,
    )
    .await;
    let pane = pane_id_of_tab(&server, &second_tab_id);
    dispatch_bell(&mut server, pane);
    let _ = recv_terminal_bells(&first_control);
    let _ = recv_terminal_bells(&second_control);

    // Model authoritative removal of the first client's selected tab. The
    // workspace fallback is build, while client 8 remains on its third tab.
    server.app.state.workspaces[0].tabs.remove(0);
    server.app.state.workspaces[0].active_tab = 0;
    server.reconcile_client_shell_locations();
    assert_eq!(
        server.shell_tab_id_for_client(7).as_deref(),
        Some(second_tab_id.as_str())
    );
    assert_eq!(
        server.shell_tab_id_for_client(8).as_deref(),
        Some(third_tab_id.as_str())
    );
    // Leaving the fallback must not revive its acknowledged marker.
    server.focus_shell_client_on_tab(7, &third_tab_id);
    server.render_and_stream();
    let first = recv_terminal_bells(&first_control);
    let second = recv_terminal_bells(&second_control);
    assert_map_value(&first, "focused_through_by_tab", &second_tab_id, 1);
    assert_map_value(&second, "focused_through_by_tab", &second_tab_id, 0);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn topology_publishes_zero_bell_additions_and_unseen_tab_removals() {
    let (mut server, _, second_tab_id) = bell_test_server();
    let (control, _render) = connect_test_shell(&mut server, 7, 80, 24);
    let _ = recv_terminal_bells(&control);
    let new_index = server.app.state.workspaces[0].test_add_tab(Some("new"));
    server.app.state.ensure_test_terminals();
    let new_tab_id = server.app.public_tab_id(0, new_index).unwrap();
    server.reconcile_client_shell_locations();
    server.render_and_stream();
    let added = recv_terminal_bells(&control);
    assert_eq!(
        added["latest_by_tab"][&new_tab_id], 0,
        "complete domain includes zero-bell additions"
    );
    let pane = pane_id_of_tab(&server, &second_tab_id);
    dispatch_bell(&mut server, pane);
    let _ = recv_terminal_bells(&control);
    let (_, removed_index) = server.app.parse_tab_id(&second_tab_id).unwrap();
    server.app.state.workspaces[0].tabs.remove(removed_index);
    server.reconcile_client_shell_locations();
    server.render_and_stream();
    let removed = recv_terminal_bells(&control);
    assert!(!removed["latest_by_tab"]
        .as_object()
        .unwrap()
        .contains_key(&second_tab_id));
    assert!(!removed["focused_through_by_tab"]
        .as_object()
        .unwrap()
        .contains_key(&second_tab_id));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn identical_facts_are_not_republished() {
    let (mut server, first_tab_id, _second_tab_id) = bell_test_server();
    let (control, _render) = connect_test_shell(&mut server, 7, 80, 24);
    let seed = recv_terminal_bells(&control);

    let pane = pane_id_of_tab(&server, &first_tab_id);
    dispatch_bell(&mut server, pane);
    server.render_and_stream();
    let update = recv_terminal_bells(&control);
    assert!(
        update["revision"].as_u64().unwrap() > seed["revision"].as_u64().unwrap(),
        "changed facts must bump the companion revision"
    );

    // No new facts: repeated renders must not publish another control.
    server.render_and_stream();
    server.render_and_stream();
    assert!(
        !terminal_bells_pending(&control),
        "unchanged facts must not be republished"
    );

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn hidden_workspace_tab_keeps_its_marker_until_visited() {
    let (mut server, first_tab_id, second_tab_id) = bell_test_server();
    let (control, _render) = connect_test_shell(&mut server, 7, 80, 24);
    let _ = recv_terminal_bells(&control);

    // Move to the second tab, then back: the first tab now behaves like a
    // background tab whose bell must survive until it is visited again.
    focus_tab_via_endpoint(&mut server, 7, "focus-second", &second_tab_id, &control).await;
    focus_tab_via_endpoint(&mut server, 7, "focus-first", &first_tab_id, &control).await;
    focus_tab_via_endpoint(
        &mut server,
        7,
        "focus-second-again",
        &second_tab_id,
        &control,
    )
    .await;
    let pane = pane_id_of_tab(&server, &first_tab_id);
    dispatch_bell(&mut server, pane);
    server.render_and_stream();

    let bells = recv_terminal_bells(&control);
    assert_map_value(&bells, "latest_by_tab", &first_tab_id, 1);
    assert_map_value(&bells, "focused_through_by_tab", &first_tab_id, 0);

    // Visiting the tab clears exactly that marker.
    focus_tab_via_endpoint(&mut server, 7, "focus-first-clear", &first_tab_id, &control).await;
    server.render_and_stream();
    let bells = recv_terminal_bells(&control);
    assert_map_value(&bells, "latest_by_tab", &first_tab_id, 1);
    assert_map_value(&bells, "focused_through_by_tab", &first_tab_id, 1);

    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn session_snapshot_exposes_terminal_bells_facts() {
    let (mut server, first_tab_id, _second_tab_id) = bell_test_server();
    let snapshot = serde_json::to_value(server.app.session_snapshot()).expect("encode snapshot");
    assert!(
        snapshot.get("terminal_bells").is_none(),
        "a fresh runtime must not claim bell facts"
    );

    let pane = pane_id_of_tab(&server, &first_tab_id);
    dispatch_bell(&mut server, pane);
    let snapshot = serde_json::to_value(server.app.session_snapshot()).expect("encode snapshot");
    let bells = snapshot["terminal_bells"]
        .as_object()
        .expect("bell facts in snapshot");
    let latest = bells["latest_by_tab"].as_object().expect("latest map");
    assert_eq!(
        latest
            .get(&first_tab_id)
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );

    shutdown_test_runtimes(&mut server);
}

#[test]
fn terminal_bells_payload_round_trips_and_old_json_defaults_absent() {
    let payload = crate::protocol::endpoint::EndpointTerminalBells {
        boot_id: "boot".into(),
        revision: 4,
        latest_by_tab: std::collections::BTreeMap::from([("workspace:t1".to_string(), 8)]),
        focused_through_by_tab: std::collections::BTreeMap::from([("workspace:t1".to_string(), 7)]),
    };
    let json = serde_json::to_string(&payload).expect("encode");
    let restored: crate::protocol::endpoint::EndpointTerminalBells =
        serde_json::from_str(&json).expect("decode");
    assert_eq!(restored, payload);

    // The public snapshot must omit the field entirely when facts are absent,
    // so older schema readers see exactly the previous JSON shape.
    let old_snapshot = serde_json::json!({
        "version": "0.1.2",
        "protocol": 16,
        "workspaces": [],
        "tabs": [],
        "panes": [],
        "layouts": [],
        "agents": []
    });
    let restored: crate::api::schema::SessionSnapshot =
        serde_json::from_value(old_snapshot).expect("old snapshot JSON stays readable");
    assert!(restored.terminal_bells.is_none());
    assert!(serde_json::to_value(&restored)
        .expect("re-encode")
        .get("terminal_bells")
        .is_none());
}
