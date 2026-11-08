use super::*;

fn scrolling_lines(range: std::ops::Range<usize>) -> Vec<u8> {
    range
        .map(|n| {
            format!(
                "build step {n:>3}: compiled module_{n} in {}ms\r\n",
                n * 7 % 97
            )
        })
        .collect::<String>()
        .into_bytes()
}

fn apply_rows(frame: &mut FrameData, rows: &[crate::protocol::PaneSurfacePatchRow]) {
    for row in rows {
        let start = usize::from(row.y) * usize::from(frame.width) + usize::from(row.x);
        frame.cells[start..start + row.cells.len()].clone_from_slice(&row.cells);
    }
}

fn scrollback_test_server(
    initial_screen: &[u8],
) -> (
    HeadlessServer,
    std::sync::mpsc::Receiver<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
    crate::layout::PaneId,
) {
    scrollback_test_server_sized(24, initial_screen)
}

fn scrollback_test_server_sized(
    rows: u16,
    initial_screen: &[u8],
) -> (
    HeadlessServer,
    std::sync::mpsc::Receiver<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
    crate::layout::PaneId,
) {
    let (mut server, control_rx, render_rx, pane_id) = retained_test_server_with_control(b"");
    server.app.state.workspaces[0].insert_test_runtime(
        pane_id,
        crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
            80,
            rows,
            100_000,
            initial_screen,
        ),
    );
    server.app.state.ensure_test_terminals();
    (server, control_rx, render_rx, pane_id)
}

#[tokio::test]
async fn shorter_client_live_tail_is_not_clipped_by_larger_terminal_viewport() {
    let (mut server, _control_rx, _render_rx, pane_id) =
        scrollback_test_server_sized(35, &scrolling_lines(0..90));
    let mut views = std::collections::HashMap::new();
    let target = Some(crate::ui::TabSurfaceTarget {
        workspace_index: 0,
        tab_index: 0,
    });
    let area = Rect::new(0, 0, 80, 24);
    let graphics = crate::kitty_graphics::surface::DeliveryCache::default();
    let render = |server: &mut HeadlessServer, views: &mut std::collections::HashMap<_, _>| {
        crate::server::client_shell::render_pane_surface(
            &mut server.app,
            target,
            area,
            false,
            false,
            crate::kitty_graphics::HostCellSize::default(),
            &graphics,
            1,
            views,
        )
        .expect("shorter client surface")
    };

    let live = render(&mut server, &mut views);
    let scroll = live.panes[0].scroll.as_ref().expect("scroll metrics");
    assert_eq!(
        scroll.viewport_rows,
        u64::from(live.panes[0].inner_rect.height)
    );
    assert!(
        frame_text(&live.frame).contains("module_89"),
        "live tail clipped"
    );

    let raw_metrics = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .unwrap()
        .scroll_metrics()
        .unwrap();
    let mut client = crate::server::clients::ClientConnection::new(
        (80, 24),
        crate::kitty_graphics::HostCellSize::default(),
        1,
        crate::protocol::RenderEncoding::SemanticFrame,
        None,
    );
    let client_metrics = raw_metrics.for_client_rows(live.panes[0].inner_rect.height as usize);
    assert_eq!(
        client_metrics.max_offset_from_bottom,
        raw_metrics.max_offset_from_bottom + 35 - 24
    );
    client.set_pane_scroll_view(pane_id, 20, client_metrics);
    views = client.pane_scroll_views;
    assert!(!frame_text(&render(&mut server, &mut views).frame).contains("module_89"));
    views.clear();
    let returned = render(&mut server, &mut views);
    assert!(frame_text(&returned.frame).contains("module_89"));
    assert_eq!(
        returned.panes[0]
            .scroll
            .as_ref()
            .unwrap()
            .offset_from_bottom,
        0
    );

    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    let visible_rows = server.shell_client_pane_viewport_rows(1, pane_id).unwrap();
    assert_eq!(visible_rows, 24);
    server.clients.get_mut(&1).unwrap().set_pane_scroll_view(
        pane_id,
        40,
        raw_metrics.for_client_rows(visible_rows),
    );
    let wheel = crate::protocol::ClientPaneInputEvent::Mouse {
        kind: crate::protocol::ClientMouseKind::ScrollDown,
        position: crate::protocol::ClientMousePosition::Cell {
            column: 30,
            row: 12,
        },
        geometry: None,
        modifiers: 0,
        lines: 3,
    };
    for _ in 0..30 {
        server.handle_server_event_with_render_impact(ServerEvent::ClientShellPaneInput {
            client_id: 1,
            pane_id: public_pane_id.clone(),
            events: vec![wheel.clone()],
        });
    }
    assert!(server.clients[&1].pane_scroll_views.is_empty());
    let mut after_views =
        std::mem::take(&mut server.clients.get_mut(&1).unwrap().pane_scroll_views);
    let live_after_wheel = render(&mut server, &mut after_views);
    assert!(frame_text(&live_after_wheel.frame).contains("module_89"));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn attached_clients_render_independent_scroll_views_of_one_terminal() {
    let (mut server, _control_rx, _render_rx, pane_id) =
        scrollback_test_server(&scrolling_lines(0..40));
    let runtime = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .expect("shared pane runtime");
    let metrics = runtime.scroll_metrics().expect("scroll metrics");
    assert!(metrics.max_offset_from_bottom > 4);

    let mut first = crate::server::clients::ClientConnection::new(
        (80, 24),
        crate::kitty_graphics::HostCellSize::default(),
        1,
        crate::protocol::RenderEncoding::SemanticFrame,
        None,
    );
    first.set_pane_scroll_view(pane_id, 4, metrics);
    let mut first_views = first.pane_scroll_views;
    let mut second_views = std::collections::HashMap::new();
    let target = Some(crate::ui::TabSurfaceTarget {
        workspace_index: 0,
        tab_index: 0,
    });
    let area = Rect::new(0, 0, 80, 24);
    let graphics = crate::kitty_graphics::surface::DeliveryCache::default();

    let first_surface = crate::server::client_shell::render_pane_surface(
        &mut server.app,
        target,
        area,
        false,
        false,
        crate::kitty_graphics::HostCellSize::default(),
        &graphics,
        1,
        &mut first_views,
    )
    .expect("first client surface");
    let second_surface = crate::server::client_shell::render_pane_surface(
        &mut server.app,
        target,
        area,
        false,
        false,
        crate::kitty_graphics::HostCellSize::default(),
        &graphics,
        2,
        &mut second_views,
    )
    .expect("second client surface");

    assert_eq!(
        first_surface.panes[0]
            .scroll
            .as_ref()
            .unwrap()
            .offset_from_bottom,
        4
    );
    assert_eq!(
        second_surface.panes[0]
            .scroll
            .as_ref()
            .unwrap()
            .offset_from_bottom,
        0
    );
    let first_text = frame_text(&first_surface.frame);
    let second_text = frame_text(&second_surface.frame);
    assert_ne!(first_text, second_text);
    assert!(
        first_text.contains("module_34"),
        "history view: {first_text:?}"
    );
    assert!(
        second_text.contains("module_39"),
        "live view: {second_text:?}"
    );
    assert_eq!(
        server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id,)
            .unwrap()
            .scroll_metrics()
            .unwrap()
            .offset_from_bottom,
        0,
        "client projection must leave the shared engine at the live tail"
    );

    let previous_max = metrics.max_offset_from_bottom;
    write_shared_test_pane(&mut server, pane_id, &scrolling_lines(40..42));
    let first_after_output = crate::server::client_shell::render_pane_surface(
        &mut server.app,
        target,
        area,
        false,
        false,
        crate::kitty_graphics::HostCellSize::default(),
        &graphics,
        1,
        &mut first_views,
    )
    .expect("first client after output");
    let second_after_output = crate::server::client_shell::render_pane_surface(
        &mut server.app,
        target,
        area,
        false,
        false,
        crate::kitty_graphics::HostCellSize::default(),
        &graphics,
        2,
        &mut second_views,
    )
    .expect("second client after output");
    let next_max = second_after_output.panes[0]
        .scroll
        .as_ref()
        .unwrap()
        .max_offset_from_bottom as usize;
    assert_eq!(
        first_after_output.panes[0]
            .scroll
            .as_ref()
            .unwrap()
            .offset_from_bottom as usize,
        4 + next_max.saturating_sub(previous_max),
        "the reading anchor should remain fixed while output advances the tail"
    );
    assert_eq!(
        second_after_output.panes[0]
            .scroll
            .as_ref()
            .unwrap()
            .offset_from_bottom,
        0,
        "the other client should continue following the tail"
    );

    first_views.clear();
    let returned = crate::server::client_shell::render_pane_surface(
        &mut server.app,
        target,
        area,
        false,
        false,
        crate::kitty_graphics::HostCellSize::default(),
        &graphics,
        1,
        &mut first_views,
    )
    .expect("first client returns to live tail");
    assert_eq!(
        returned.panes[0]
            .scroll
            .as_ref()
            .unwrap()
            .offset_from_bottom,
        0
    );
    assert_eq!(
        second_views.get(&pane_id),
        None,
        "one client's return-to-tail action must not alter the other attachment"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn image_placements_follow_each_clients_text_viewport() {
    let mut output = scrolling_lines(0..10);
    output.extend_from_slice(b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\");
    output.extend_from_slice(&scrolling_lines(10..40));
    let (mut server, _control_rx, _render_rx, pane_id) = scrollback_test_server(&output);
    let metrics = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .unwrap()
        .scroll_metrics()
        .unwrap();
    assert!(metrics.max_offset_from_bottom > 0);
    let mut history_views = std::collections::HashMap::new();
    let mut history_client = crate::server::clients::ClientConnection::new(
        (80, 24),
        crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        },
        1,
        crate::protocol::RenderEncoding::SemanticFrame,
        None,
    );
    history_client.set_pane_scroll_view(pane_id, metrics.max_offset_from_bottom, metrics);
    history_views.extend(history_client.pane_scroll_views);
    let target = Some(crate::ui::TabSurfaceTarget {
        workspace_index: 0,
        tab_index: 0,
    });
    let cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    let delivery = crate::kitty_graphics::surface::DeliveryCache::default();
    let history = crate::server::client_shell::render_pane_surface(
        &mut server.app,
        target,
        Rect::new(0, 0, 80, 24),
        false,
        false,
        cell_size,
        &delivery,
        1,
        &mut history_views,
    )
    .expect("history graphics surface");
    let live = crate::server::client_shell::render_pane_surface(
        &mut server.app,
        target,
        Rect::new(0, 0, 80, 24),
        false,
        false,
        cell_size,
        &delivery,
        2,
        &mut std::collections::HashMap::new(),
    )
    .expect("live graphics surface");
    assert_eq!(history.graphics.placements.len(), 1);
    assert!(live.graphics.placements.is_empty());
    assert_eq!(
        server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id,)
            .unwrap()
            .scroll_metrics()
            .unwrap()
            .offset_from_bottom,
        0,
        "graphics projection must restore the shared engine viewport"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn shorter_client_image_placement_uses_the_same_cropped_rows_as_text() {
    let mut output = scrolling_lines(0..25);
    output.extend_from_slice(b"\x1b_Ga=T,f=32,t=d,i=8,p=4,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\");
    output.extend_from_slice(&scrolling_lines(25..40));
    let (mut server, _control_rx, _render_rx, pane_id) = scrollback_test_server_sized(35, &output);
    let runtime = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .unwrap();
    let full = runtime.kitty_image_placements_at_scroll_offset_with_data_filter(0, 35, |_| true);
    let short = runtime.kitty_image_placements_at_scroll_offset_with_data_filter(0, 24, |_| true);
    assert_eq!(full.len(), 1);
    assert_eq!(short.len(), 1);
    assert_eq!(
        short[0].render.viewport_row,
        full[0].render.viewport_row - 11
    );
    assert_eq!(runtime.scroll_metrics().unwrap().offset_from_bottom, 0);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn shorter_client_hyperlinks_use_cropped_viewport_coordinates() {
    let mut output = scrolling_lines(0..25);
    output.extend_from_slice(b"\x1b]8;;https://example.invalid\x1b\\LINK\x1b]8;;\x1b\\\r\n");
    output.extend_from_slice(&scrolling_lines(25..40));
    let (mut server, _control_rx, _render_rx, pane_id) = scrollback_test_server_sized(35, &output);
    let runtime = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .unwrap();
    let full = runtime.visible_hyperlinks_at_scroll_offset(Rect::new(0, 0, 80, 35), 0);
    let short = runtime.visible_hyperlinks_at_scroll_offset(Rect::new(0, 0, 80, 24), 0);
    assert!(!full.is_empty(), "link fixture should be visible");
    assert_eq!(short.len(), full.len());
    assert_eq!((short[0].0).1, (full[0].0).1 - 11);
    assert_eq!(runtime.scroll_metrics().unwrap().offset_from_bottom, 0);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_shell_pane_scroll_endpoint_changes_only_requesting_attachment() {
    let (mut server, control_rx, _render_rx, pane_id) =
        scrollback_test_server(&scrolling_lines(0..40));
    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    assert!(
        crate::server::client_commands::supports_client_shell_method(
            &crate::api::schema::Method::PaneScroll(crate::api::schema::PaneScrollParams {
                pane_id: public_pane_id.clone(),
                offset_from_bottom: 5,
            }),
        )
    );
    assert!(server
        .app
        .client_pane_scroll_info(&public_pane_id)
        .is_some());
    assert!(server.shell_client_views_pane(1, 0, pane_id));
    let runtime = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .unwrap();
    assert!(runtime.scroll_metrics().unwrap().max_offset_from_bottom >= 5);
    server.clients.insert(
        2,
        crate::server::clients::ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            crate::protocol::RenderEncoding::SemanticFrame,
            None,
        ),
    );

    assert!(server.handle_client_shell_endpoint_request(
        1,
        server.client_shell_boot_id.clone(),
        Box::new(crate::api::schema::Request {
            id: "scroll-first-only".to_owned(),
            method: crate::api::schema::Method::PaneScroll(crate::api::schema::PaneScrollParams {
                pane_id: public_pane_id,
                offset_from_bottom: 5,
            },),
        }),
    ));
    let response = read_server_message(
        control_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("client-local scroll response"),
    );
    let ServerMessage::ClientShellEndpointResponseChunk { data, .. } = response else {
        panic!("expected endpoint response");
    };
    let success = serde_json::from_slice::<crate::api::schema::SuccessResponse>(&data).unwrap();
    assert!(matches!(
        success.result,
        crate::api::schema::ResponseResult::PaneInfo { .. }
    ));
    let metrics = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .unwrap()
        .scroll_metrics()
        .unwrap();
    assert_eq!(
        server
            .clients
            .get_mut(&1)
            .unwrap()
            .reconcile_pane_scroll_view(pane_id, metrics)
            .offset_from_bottom,
        5,
    );
    assert!(server.clients[&2].pane_scroll_views.is_empty());
    assert_eq!(
        server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .unwrap()
            .scroll_metrics()
            .unwrap()
            .offset_from_bottom,
        0,
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn resized_history_reader_can_scroll_back_to_live_tail() {
    let mut output = scrolling_lines(0..180);
    output.extend_from_slice(b"M3_FOLLOW_NEW\r\n");
    let (mut server, _control_rx, _render_rx, pane_id) = scrollback_test_server(&output);
    let public_pane_id = server.app.public_pane_id(0, pane_id).unwrap();
    let metrics = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .unwrap()
        .scroll_metrics()
        .unwrap();
    assert!(metrics.max_offset_from_bottom > 70);
    server
        .clients
        .get_mut(&1)
        .unwrap()
        .set_pane_scroll_view(pane_id, 70, metrics);
    server.clients.insert(
        2,
        crate::server::clients::ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            crate::protocol::RenderEncoding::SemanticFrame,
            None,
        ),
    );
    assert!(server.handle_server_event(ServerEvent::ClientShellResize {
        client_id: 1,
        surface_cols: 110,
        surface_rows: 30,
        cell_width_px: 10,
        cell_height_px: 20,
        pixel_mouse: false,
    }));
    let after_resize = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .unwrap()
        .scroll_metrics()
        .unwrap();
    assert!(
        server
            .clients
            .get_mut(&1)
            .unwrap()
            .reconcile_pane_scroll_view(pane_id, after_resize)
            .offset_from_bottom
            > 0
    );

    let wheel = crate::protocol::ClientPaneInputEvent::Mouse {
        kind: crate::protocol::ClientMouseKind::ScrollDown,
        position: crate::protocol::ClientMousePosition::Cell {
            column: 50,
            row: 12,
        },
        geometry: None,
        modifiers: 0,
        lines: 3,
    };
    for _ in 0..100 {
        server.handle_server_event_with_render_impact(ServerEvent::ClientShellPaneInput {
            client_id: 1,
            pane_id: public_pane_id.clone(),
            events: vec![wheel.clone()],
        });
    }
    assert!(server.clients[&1].pane_scroll_views.is_empty());
    assert!(server.clients[&2].pane_scroll_views.is_empty());
    let target = Some(crate::ui::TabSurfaceTarget {
        workspace_index: 0,
        tab_index: 0,
    });
    let surface = crate::server::client_shell::render_pane_surface(
        &mut server.app,
        target,
        Rect::new(0, 0, 110, 30),
        false,
        false,
        crate::kitty_graphics::HostCellSize::default(),
        &crate::kitty_graphics::surface::DeliveryCache::default(),
        1,
        &mut server.clients.get_mut(&1).unwrap().pane_scroll_views,
    )
    .unwrap();
    assert!(frame_text(&surface.frame).contains("M3_FOLLOW_NEW"));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn retained_pane_patches_fall_back_when_a_client_reads_history() {
    let (mut server, _control_rx, render_rx, pane_id) =
        scrollback_test_server(&scrolling_lines(0..40));
    server.render_and_stream();
    let _ = render_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let metrics = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
        .unwrap()
        .scroll_metrics()
        .unwrap();
    server
        .clients
        .get_mut(&1)
        .unwrap()
        .set_pane_scroll_view(pane_id, 2, metrics);

    write_shared_test_pane(&mut server, pane_id, &scrolling_lines(40..42));
    assert!(
        !server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])),
        "shared-engine dirty rows cannot safely update a non-tail client view"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn surface_scroll_sends_scrolling_output_as_a_shift_and_new_rows() {
    let (mut server, _control_rx, render_rx, pane_id) =
        retained_test_server_with_control(&scrolling_lines(0..40));
    server
        .clients
        .get_mut(&1)
        .expect("scroll client")
        .render_state
        .enable_surface_scroll(true);
    server.render_and_stream();
    let mut decoder = protocol::surface_reuse::Decoder::new(false, true);
    let ServerMessage::PaneSurface(mut shell) = decoder
        .decode(read_server_message(
            render_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        ))
        .expect("initial surface")
    else {
        panic!("expected the initial pane surface");
    };

    write_shared_test_pane(&mut server, pane_id, &scrolling_lines(40..42));
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    let bytes = render_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let message = read_server_message(bytes.clone());
    assert!(matches!(
        &message,
        ServerMessage::EndpointControl { kind, .. } if kind == protocol::surface_scroll::MESSAGE_KIND
    ));

    // The decoder hands the client shell an ordinary patch that reaches the
    // server's committed surface exactly.
    let ServerMessage::PaneSurfacePatch(patch) = decoder.decode(message).expect("scroll decode")
    else {
        panic!("expected an expanded pane patch");
    };
    apply_rows(&mut shell.frame, &patch.rows);
    let committed = server.clients[&1]
        .render_state
        .last_pane_surface()
        .expect("committed surface");
    assert_eq!(shell.frame.cells, committed.frame.cells);
    assert!(frame_text(&shell.frame).contains("module_41"));

    let expanded = HeadlessServer::frame_server_message(&ServerMessage::PaneSurfacePatch(patch))
        .expect("expanded frame");
    assert!(
        bytes.len() * 4 < expanded.len(),
        "scroll frame {} should be far smaller than the rows it replaces {}",
        bytes.len(),
        expanded.len()
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn surface_scroll_is_not_sent_to_a_peer_that_did_not_negotiate_it() {
    let (mut server, _control_rx, render_rx, pane_id) =
        retained_test_server_with_control(&scrolling_lines(0..40));
    server.render_and_stream();
    let _ = render_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    write_shared_test_pane(&mut server, pane_id, &scrolling_lines(40..42));
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    let message = read_server_message(render_rx.recv_timeout(Duration::from_secs(1)).unwrap());
    assert!(matches!(message, ServerMessage::PaneSurfacePatch(_)));
    shutdown_test_runtimes(&mut server);
}
