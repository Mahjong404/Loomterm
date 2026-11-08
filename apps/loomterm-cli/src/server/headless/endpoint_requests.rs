// Modified for Loomterm (2026-11-05); see apps/loomterm-cli/PATCHES.md.
use super::*;

impl HeadlessServer {
    pub(super) fn handle_client_shell_endpoint_request(
        &mut self,
        client_id: u64,
        boot_id: String,
        mut request: Box<api::schema::Request>,
    ) -> bool {
        let Some(client) = self.clients.get(&client_id) else {
            return false;
        };
        if !matches!(client.mode, ClientConnectionMode::ClientShell) {
            self.remove_client_and_resize_if_needed(client_id);
            return true;
        }
        let request_id = request.id.clone();
        if !crate::server::client_commands::supports_client_shell_method(&request.method) {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "unsupported_endpoint_command",
                "this method is not available through the client shell command lane",
            );
            self.send_to_client(client_id, message);
            return false;
        }
        if boot_id != self.client_shell_boot_id {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "stale_boot",
                "endpoint command targeted an earlier server boot",
            );
            self.send_to_client(client_id, message);
            return false;
        }
        let surface_active = client.shell_surface_active;
        if let api::schema::Method::ClientShellSurfaceSet(params) = &request.method {
            let Some((changed, projection_revision)) =
                self.set_client_shell_surface_active(client_id, params.active)
            else {
                return false;
            };
            self.send_to_client(
                client_id,
                crate::server::client_commands::success_message_with_result(
                    boot_id,
                    request_id,
                    api::schema::ResponseResult::ClientShellSurfaceSet {
                        active: params.active,
                        projection_revision,
                    },
                ),
            );
            return changed;
        }
        if client.shell_endpoint_command_in_flight {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "endpoint_busy",
                "this endpoint is still processing another command",
            );
            self.send_to_client(client_id, message);
            return false;
        }
        if !surface_active {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "surface_inactive",
                "this method requires an active client shell surface",
            );
            self.send_to_client(client_id, message);
            return false;
        }

        if let api::schema::Method::PaneScroll(params) = &request.method {
            return self.handle_client_shell_pane_scroll(
                client_id,
                boot_id,
                request_id,
                params.clone(),
            );
        }

        let api_request_id = format!(
            "endpoint:{}:{client_id}:{request_id}",
            self.client_shell_boot_id
        );
        request.id = api_request_id.clone();
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        if let Err(err) = crate::server::client_commands::spawn_response_waiter(
            client_id,
            boot_id.clone(),
            request_id.clone(),
            response_rx,
            self.server_event_tx.clone(),
        ) {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "server_unavailable",
                format!("failed to start endpoint response bridge: {err}"),
            );
            self.send_to_client(client_id, message);
            return false;
        }
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.shell_endpoint_command_in_flight = true;
            // A later source restore has a new projection revision. Keep this request's lease
            // so a delayed worktree response cannot focus a pane after endpoint switching.
            client.shell_endpoint_command_surface_revision = Some(client.shell_projection_revision);
            let deferred_worktree = matches!(
                &request.method,
                api::schema::Method::WorktreeCreate(_)
                    | api::schema::Method::WorktreeRemove(_)
                    | api::schema::Method::WorktreeList(_)
                    | api::schema::Method::WorktreeOpen(_)
            );
            let deferred_navigation = match &request.method {
                api::schema::Method::WorktreeCreate(params) => params.focus,
                api::schema::Method::WorktreeOpen(params) => params.focus,
                _ => false,
            };
            client.shell_deferred_navigation_request_id =
                deferred_worktree.then(|| api_request_id.clone());
            client.shell_deferred_navigation_response = deferred_navigation.then(Vec::new);
        }
        let message = match api::ApiRequestMessage::try_new(*request, respond_to, None) {
            Ok(message) => message,
            Err(reason) => {
                self.send_to_client(
                    client_id,
                    crate::server::client_commands::error_message(
                        boot_id,
                        request_id,
                        "server_busy",
                        reason,
                    ),
                );
                return false;
            }
        };
        let foreground_changed = self.promote_client_to_foreground(client_id);
        foreground_changed | self.handle_client_shell_api_request(client_id, message)
    }

    fn handle_client_shell_pane_scroll(
        &mut self,
        client_id: u64,
        boot_id: String,
        request_id: String,
        params: api::schema::PaneScrollParams,
    ) -> bool {
        let Some((pane_id, mut pane, metrics)) = self.app.client_pane_scroll_info(&params.pane_id)
        else {
            self.send_to_client(
                client_id,
                crate::server::client_commands::error_message(
                    boot_id,
                    request_id,
                    "pane_not_found",
                    format!("pane {} is not available", params.pane_id),
                ),
            );
            return false;
        };
        let Some((workspace_index, _)) = self.app.parse_pane_id(&params.pane_id) else {
            return false;
        };
        if !self.shell_client_views_pane(client_id, workspace_index, pane_id) {
            self.send_to_client(
                client_id,
                crate::server::client_commands::error_message(
                    boot_id,
                    request_id,
                    "pane_not_visible",
                    "this client is not viewing the targeted pane",
                ),
            );
            return false;
        }
        let metrics = metrics.for_client_rows(
            self.shell_client_pane_viewport_rows(client_id, pane_id)
                .unwrap_or(metrics.viewport_rows),
        );
        let requested = usize::try_from(params.offset_from_bottom).unwrap_or(usize::MAX);
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        let metrics = client.set_pane_scroll_view(pane_id, requested, metrics);
        pane.scroll = Some(api::schema::PaneScrollInfo {
            offset_from_bottom: metrics.offset_from_bottom as u64,
            max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
            viewport_rows: metrics.viewport_rows as u64,
        });
        client.request_recompute();
        self.app.render_dirty.request_generic();
        self.send_to_client(
            client_id,
            crate::server::client_commands::success_message_with_result(
                boot_id,
                request_id,
                api::schema::ResponseResult::PaneInfo { pane },
            ),
        );
        true
    }
}
