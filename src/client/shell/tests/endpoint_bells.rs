use super::*;

fn two_tab_snapshot() -> ClientShellSnapshot {
    let mut projected = snapshot();
    projected.tabs = ["api", "build"]
        .iter()
        .enumerate()
        .map(|(index, label)| ClientShellTab {
            tab_id: format!("ws_1:t{}", index + 1),
            workspace_id: "ws_1".into(),
            number: index + 1,
            label: (*label).into(),
            custom_label: true,
            zoomed: false,
            focused: index == 0,
            agent_status: AgentStatus::Idle,
        })
        .collect();
    projected.focused_tab_id = Some("ws_1:t1".into());
    projected
}

fn shell_config() -> ClientShellConfig {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.mobile_width_threshold = 0;
    config
}

fn shell_with_two_tabs() -> ClientShellState {
    let mut state = ClientShellState::new(shell_config());
    state.set_snapshot(Box::new(two_tab_snapshot()));
    state.set_pane_surface(surface());
    state.receive_endpoint_terminal_bells(
        &ClientEndpointId::Local,
        None,
        terminal_bells("boot-1", 0, &[("ws_1:t1", 0), ("ws_1:t2", 0)], &[]),
    );
    state
}

fn terminal_bells(
    boot_id: &str,
    revision: u64,
    latest: &[(&str, u64)],
    frontier: &[(&str, u64)],
) -> crate::protocol::endpoint::EndpointTerminalBells {
    crate::protocol::endpoint::EndpointTerminalBells {
        boot_id: boot_id.into(),
        revision,
        latest_by_tab: latest
            .iter()
            .map(|(tab, serial)| ((*tab).to_string(), *serial))
            .collect(),
        focused_through_by_tab: frontier
            .iter()
            .map(|(tab, serial)| ((*tab).to_string(), *serial))
            .collect(),
    }
}

fn tab_bar_row(frame: &FrameData, rect: Rect) -> String {
    (rect.x..rect.right())
        .map(|x| {
            frame.cells[(rect.y * frame.width + x) as usize]
                .symbol
                .as_str()
        })
        .collect()
}

fn tab_text(state: &ClientShellState, frame: &FrameData, tab_id: &str) -> String {
    let rect = state
        .hits
        .tabs
        .iter()
        .find(|(_, id)| id == tab_id)
        .unwrap_or_else(|| panic!("{tab_id} tab hit"))
        .0;
    tab_bar_row(frame, rect)
}

#[test]
fn background_tab_bell_marks_tab_until_focused() {
    let mut state = shell_with_two_tabs();
    let endpoint = ClientEndpointId::Local;
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            1,
            &[("ws_1:t1", 0), ("ws_1:t2", 1)],
            &[("ws_1:t1", 1)],
        ),
    );
    let frame = state.compose(60, 20).expect("bell frame");
    let build_text = tab_text(&state, &frame, "ws_1:t2");
    let api_text = tab_text(&state, &frame, "ws_1:t1");
    assert!(
        build_text.contains("! build"),
        "background bell marks build tab: {build_text:?}"
    );
    assert!(
        !api_text.contains('!'),
        "viewed tab stays unmarked: {api_text:?}"
    );

    // Focusing the tab commits the server frontier; the marker clears.
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            2,
            &[("ws_1:t1", 0), ("ws_1:t2", 1)],
            &[("ws_1:t1", 1), ("ws_1:t2", 1)],
        ),
    );
    let frame = state.compose(60, 20).expect("acknowledged frame");
    let build_text = tab_text(&state, &frame, "ws_1:t2");
    assert!(
        !build_text.contains('!'),
        "frontier through latest clears marker: {build_text:?}"
    );
}

#[test]
fn frontier_from_focus_before_bell_suppresses_marker_for_that_serial() {
    let mut state = shell_with_two_tabs();
    let endpoint = ClientEndpointId::Local;
    // The connection visited the tab at serial 1 and left; a bell dispatched
    // while it was viewed is already acknowledged at that frontier.
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            1,
            &[("ws_1:t1", 0), ("ws_1:t2", 1)],
            &[("ws_1:t1", 1), ("ws_1:t2", 1)],
        ),
    );
    let frame = state.compose(60, 20).expect("suppressed frame");
    let build_text = tab_text(&state, &frame, "ws_1:t2");
    assert!(
        !build_text.contains('!'),
        "bell through frontier stays unmarked: {build_text:?}"
    );

    // A bell dispatched after the visit exceeds the frontier and marks.
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            2,
            &[("ws_1:t1", 0), ("ws_1:t2", 2)],
            &[("ws_1:t1", 1), ("ws_1:t2", 1)],
        ),
    );
    let frame = state.compose(60, 20).expect("marked frame");
    let build_text = tab_text(&state, &frame, "ws_1:t2");
    assert!(
        build_text.contains("! build"),
        "later bell beyond frontier marks: {build_text:?}"
    );
}

#[test]
fn intermediate_focus_visits_survive_snapshot_coalescing() {
    // Facts arrive through the replaceable companion slot, so intermediate
    // projections can be dropped entirely. The final full projection still
    // carries the visit's frontier; the marker is decided by latest > frontier,
    // not by which tab the cached snapshot happens to show.
    let mut state = shell_with_two_tabs();
    let endpoint = ClientEndpointId::Local;
    // Bell on the viewed tab at serial 1 (no marker), then focus moved to the
    // other tab and a later bell rang there at serial 2.
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            2,
            &[("ws_1:t1", 2), ("ws_1:t2", 1)],
            &[("ws_1:t1", 1), ("ws_1:t2", 1)],
        ),
    );
    let mut focused = state.snapshot.as_deref().expect("snapshot").clone();
    focused.revision = 2;
    focused.focused_tab_id = Some("ws_1:t2".into());
    for tab in &mut focused.tabs {
        tab.focused = tab.tab_id == "ws_1:t2";
    }
    state.set_snapshot(Box::new(focused));
    let mut focused_surface = surface();
    focused_surface.projection_revision = 2;
    state.set_pane_surface(focused_surface);
    let frame = state.compose(60, 20).expect("coalesced frame");
    let api_text = tab_text(&state, &frame, "ws_1:t1");
    let build_text = tab_text(&state, &frame, "ws_1:t2");
    assert!(
        api_text.contains("! api"),
        "unseen bell marks api even after transient focus: {api_text:?}"
    );
    assert!(
        !build_text.contains('!'),
        "build's earlier bell stayed acknowledged: {build_text:?}"
    );
}

#[test]
fn hidden_workspace_tabs_keep_markers_until_visited() {
    let mut projected = two_tab_snapshot();
    let template_workspace = projected.workspaces[0].clone();
    projected.workspaces.push(ClientShellWorkspace {
        workspace_id: "ws_2".into(),
        label: "hidden".into(),
        active_tab_id: "ws_2:t1".into(),
        focused: false,
        ..template_workspace
    });
    projected.tabs.push(ClientShellTab {
        tab_id: "ws_2:t1".into(),
        workspace_id: "ws_2".into(),
        number: 1,
        label: "deploy".into(),
        custom_label: true,
        zoomed: false,
        focused: false,
        agent_status: AgentStatus::Idle,
    });
    let mut state = ClientShellState::new(shell_config());
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    let endpoint = ClientEndpointId::Local;
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            0,
            &[("ws_1:t1", 0), ("ws_1:t2", 0), ("ws_2:t1", 0)],
            &[],
        ),
    );
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            1,
            &[("ws_1:t1", 0), ("ws_1:t2", 1), ("ws_2:t1", 2)],
            &[("ws_1:t1", 2)],
        ),
    );
    let frame = state.compose(60, 20).expect("hidden workspace frame");
    // Only the focused workspace's tabs render; the hidden-workspace tab's
    // facts are retained even though nothing draws them yet.
    assert!(state.hits.tabs.iter().all(|(_, id)| id != "ws_2:t1"));
    let build_text = tab_text(&state, &frame, "ws_1:t2");
    assert!(
        build_text.contains("! build"),
        "visible background bell marks: {build_text:?}"
    );

    // Switch into the hidden workspace: the retained marker renders, because
    // the connection never committed focus into that tab.
    let mut switched = state.snapshot.as_deref().expect("snapshot").clone();
    switched.revision = 2;
    switched.focused_workspace_id = Some("ws_2".into());
    switched.focused_tab_id = Some("ws_2:t1".into());
    switched.focused_pane_id = None;
    for workspace in &mut switched.workspaces {
        workspace.focused = workspace.workspace_id == "ws_2";
    }
    for tab in &mut switched.tabs {
        tab.focused = tab.tab_id == "ws_2:t1";
    }
    state.set_snapshot(Box::new(switched));
    let mut switched_surface = surface();
    switched_surface.projection_revision = 2;
    state.set_pane_surface(switched_surface);
    let frame = state.compose(60, 20).expect("workspace switch frame");
    let deploy_text = tab_text(&state, &frame, "ws_2:t1");
    // Focused tabs never draw a marker; the committed-focus frontier arrives
    // with the next companion update and acknowledges the visit.
    assert!(
        deploy_text.contains("deploy"),
        "hidden tab renders on visit: {deploy_text:?}"
    );
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            2,
            &[("ws_1:t1", 0), ("ws_1:t2", 1), ("ws_2:t1", 2)],
            &[("ws_1:t1", 2), ("ws_2:t1", 2)],
        ),
    );
    assert!(
        !state
            .endpoints
            .iter()
            .find(|entry| entry.endpoint_id == endpoint)
            .expect("endpoint")
            .terminal_bells
            .owes_marker("ws_2:t1"),
        "visiting the hidden tab acknowledges its bells"
    );
}

#[test]
fn markers_are_per_endpoint_and_survive_cached_switching() {
    let mut state = ClientShellState::new(shell_config());
    let remote = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
    );
    let profile = SavedSshEndpoint {
        id: match &remote {
            ClientEndpointId::Ssh(id) => id.clone(),
            _ => unreachable!(),
        },
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(two_tab_snapshot()));
    state.set_pane_surface(surface());
    let mut remote_snapshot = two_tab_snapshot();
    remote_snapshot.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot(&remote, Box::new(remote_snapshot));
    state.receive_endpoint_terminal_bells(
        &ClientEndpointId::Local,
        Some(3),
        terminal_bells("boot-1", 0, &[("ws_1:t1", 0), ("ws_1:t2", 0)], &[]),
    );
    state.receive_endpoint_terminal_bells(
        &remote,
        Some(7),
        terminal_bells("remote-boot", 0, &[("ws_1:t1", 0), ("ws_1:t2", 0)], &[]),
    );

    // The remote connection acknowledged build's bell; local has not.
    state.receive_endpoint_terminal_bells(
        &remote,
        Some(7),
        terminal_bells(
            "remote-boot",
            1,
            &[("ws_1:t1", 0), ("ws_1:t2", 1)],
            &[("ws_1:t1", 1), ("ws_1:t2", 1)],
        ),
    );
    state.receive_endpoint_terminal_bells(
        &ClientEndpointId::Local,
        Some(3),
        terminal_bells(
            "boot-1",
            1,
            &[("ws_1:t1", 0), ("ws_1:t2", 1)],
            &[("ws_1:t1", 1)],
        ),
    );

    // Facts are installed per endpoint; the local marker exists while the
    // remote one is already acknowledged.
    let owes = |state: &ClientShellState, endpoint_id: &ClientEndpointId| {
        state
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .expect("endpoint")
            .terminal_bells
            .owes_marker("ws_1:t2")
    };
    assert!(owes(&state, &ClientEndpointId::Local));
    assert!(!owes(&state, &remote));

    // The exact receive path used by control dispatch must store inactive
    // endpoint facts, but must not request repaint of the selected endpoint.
    assert!(!state.endpoint_is_active(&remote));
    assert!(!state.receive_endpoint_terminal_bells(
        &remote,
        Some(7),
        terminal_bells(
            "remote-boot",
            2,
            &[("ws_1:t1", 0), ("ws_1:t2", 2)],
            &[("ws_1:t2", 1)]
        ),
    ));
    assert!(owes(&state, &remote), "inactive endpoint retains new bells");
}

#[test]
fn fresh_seed_baselines_bells_and_same_boot_reconnect_preserves_unread() {
    let mut presentation =
        crate::client::shell::endpoint_bells::EndpointTerminalBellsPresentation::default();
    presentation.receive(Some(1), terminal_bells("boot-1", 0, &[("build", 5)], &[]));
    assert!(
        !presentation.owes_marker("build"),
        "fresh seed must not replay old bells"
    );
    presentation.receive(Some(1), terminal_bells("boot-1", 1, &[("build", 6)], &[]));
    assert!(presentation.owes_marker("build"));
    presentation.receive(Some(2), terminal_bells("boot-1", 0, &[("build", 6)], &[]));
    assert!(
        presentation.owes_marker("build"),
        "same-boot reconnect keeps unread bells"
    );
}

#[test]
fn new_boot_rebaselines_and_stale_generations_are_ignored() {
    let mut state = shell_with_two_tabs();
    let endpoint = ClientEndpointId::Local;
    state.receive_endpoint_terminal_bells(
        &endpoint,
        Some(1),
        terminal_bells(
            "boot-1",
            3,
            &[("ws_1:t1", 0), ("ws_1:t2", 2)],
            &[("ws_1:t1", 2), ("ws_1:t2", 2)],
        ),
    );
    // Same transport generation, older revision: ignored.
    state.receive_endpoint_terminal_bells(
        &endpoint,
        Some(1),
        terminal_bells("boot-1", 1, &[("ws_1:t1", 0), ("ws_1:t2", 1)], &[]),
    );
    // Newer generation reconnect, same boot: held acknowledgements survive and
    // the seed's lower revision is still accepted for the new connection.
    state.receive_endpoint_terminal_bells(
        &endpoint,
        Some(2),
        terminal_bells(
            "boot-1",
            0,
            &[("ws_1:t1", 0), ("ws_1:t2", 2)],
            &[("ws_1:t1", 2)],
        ),
    );
    let presentation = &state
        .endpoints
        .iter()
        .find(|entry| entry.endpoint_id == endpoint)
        .expect("endpoint")
        .terminal_bells;
    assert!(
        !presentation.owes_marker("ws_1:t2"),
        "reconnect keeps the seen watermark from the earlier generation"
    );

    // A new boot baselines its seed, including bells emitted before this
    // client attached. Only subsequent bells may produce a new marker.
    state.receive_endpoint_terminal_bells(
        &endpoint,
        Some(2),
        terminal_bells(
            "boot-2",
            0,
            &[("ws_1:t1", 0), ("ws_1:t2", 1)],
            &[("ws_1:t1", 1)],
        ),
    );
    let frame = state.compose(60, 20).expect("new boot frame");
    let build_text = tab_text(&state, &frame, "ws_1:t2");
    assert!(
        !build_text.contains('!'),
        "new boot seed does not replay prior bells: {build_text:?}"
    );
    state.receive_endpoint_terminal_bells(
        &endpoint,
        Some(2),
        terminal_bells("boot-2", 1, &[("ws_1:t1", 0), ("ws_1:t2", 2)], &[]),
    );
    let frame = state.compose(60, 20).expect("new boot bell frame");
    assert!(tab_text(&state, &frame, "ws_1:t2").contains("! build"));
}

#[test]
fn narrow_tab_widths_keep_the_leading_bang() {
    let mut state = shell_with_two_tabs();
    let endpoint = ClientEndpointId::Local;
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            1,
            &[("ws_1:t1", 0), ("ws_1:t2", 1)],
            &[("ws_1:t1", 1)],
        ),
    );
    let frame = state.compose(40, 20).expect("narrow frame");
    let build_text = tab_text(&state, &frame, "ws_1:t2");
    assert!(
        build_text.starts_with('!'),
        "marker stays at the leading cell when clipped: {build_text:?}"
    );
}

#[test]
fn tab_removal_prunes_facts_and_missing_metadata_marks_nothing() {
    let mut state = ClientShellState::new(shell_config());
    state.set_snapshot(Box::new(two_tab_snapshot()));
    state.set_pane_surface(surface());
    let endpoint = ClientEndpointId::Local;
    // No companion received at all: nothing marks.
    let frame = state.compose(60, 20).expect("no metadata frame");
    let build_text = tab_text(&state, &frame, "ws_1:t2");
    assert!(
        !build_text.contains('!'),
        "absent companion facts mark nothing: {build_text:?}"
    );
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells("boot-1", 0, &[("ws_1:t1", 0), ("ws_1:t2", 0)], &[]),
    );

    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            1,
            &[("ws_1:t1", 0), ("ws_1:t2", 1)],
            &[("ws_1:t1", 1)],
        ),
    );
    // The tab disappears from the live domain; the full replacement drops it
    // and the retained watermark is pruned with it.
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells("boot-1", 2, &[("ws_1:t1", 0)], &[("ws_1:t1", 1)]),
    );
    let presentation = &state
        .endpoints
        .iter()
        .find(|entry| entry.endpoint_id == endpoint)
        .expect("endpoint")
        .terminal_bells;
    assert!(
        !presentation.owes_marker("ws_1:t2"),
        "removed tab prunes latest and watermark"
    );

    // A re-added tab with no bells starts clean (latest 0, no marker).
    state.receive_endpoint_terminal_bells(
        &endpoint,
        None,
        terminal_bells(
            "boot-1",
            3,
            &[("ws_1:t1", 0), ("ws_1:t2", 0)],
            &[("ws_1:t1", 1)],
        ),
    );
    let frame = state.compose(60, 20).expect("readded frame");
    let build_text = tab_text(&state, &frame, "ws_1:t2");
    assert!(
        !build_text.contains('!'),
        "re-added tab without bells is unmarked: {build_text:?}"
    );
}
