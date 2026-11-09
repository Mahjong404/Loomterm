// Modified for Loomterm (2026-11-05); see apps/loomterm-cli/PATCHES.md.
pub mod client;
mod event_hub;
pub mod schema;
mod server;
mod status;
mod subscriptions;
mod wait;

pub use event_hub::EventHub;
pub use server::ServerHandle;
pub(crate) use server::{api_method_name, start_server_with_stop_control};
pub use status::{read_runtime_status_at, RuntimeStatus};

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::api::schema::Request;

pub const SOCKET_PATH_ENV_VAR: &str = "LOOMTERM_SOCKET_PATH";

pub(crate) fn request_changes_ui(request: &Request) -> bool {
    request.method.operation().changes_ui
}

pub struct ApiRequestMessage {
    pub request: Request,
    pub respond_to: std::sync::mpsc::Sender<String>,
    pub response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
    pub(crate) _admission: Option<crate::pty::budget::Permit>,
}

impl ApiRequestMessage {
    pub(crate) fn try_new(
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
        response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
    ) -> Result<Self, &'static str> {
        use crate::pty::budget::Budget;
        static BUDGET: std::sync::OnceLock<std::sync::Arc<Budget>> = std::sync::OnceLock::new();
        let bytes = serde_json::to_vec(&request)
            .map_err(|_| "request serialization failed")?
            .len();
        if bytes > 1024 * 1024 {
            return Err("request exceeds 1 MiB");
        }
        let permit = BUDGET
            .get_or_init(|| Budget::new(64, 16 * 1024 * 1024))
            .acquire(bytes)
            .ok_or("API request budget exhausted")?;
        Ok(Self {
            request,
            respond_to,
            response_write_complete,
            _admission: Some(permit),
        })
    }
}

pub type ApiRequestSender = mpsc::UnboundedSender<ApiRequestMessage>;

pub fn socket_path() -> PathBuf {
    crate::session::active_api_socket_path()
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::pty::budget::Budget;

    fn ping() -> Request {
        Request {
            id: "bounded".into(),
            method: schema::Method::Ping(schema::PingParams::default()),
        }
    }

    #[test]
    fn m1_api_admission_survives_caller_timeout_and_deferred_dispatch() {
        let budget = Budget::new(1, 64);
        let (respond_to, response) = std::sync::mpsc::channel();
        let message = ApiRequestMessage {
            request: ping(),
            respond_to,
            response_write_complete: None,
            _admission: budget.acquire(64),
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        sender.send(message).unwrap();
        drop(response);
        let deferred = vec![receiver.try_recv().unwrap()];
        assert!(
            budget.acquire(0).is_none(),
            "caller departure and queue transfer retain admission"
        );
        drop(deferred);
        assert!(budget.acquire(64).is_some());
    }

    #[test]
    fn m1_api_oversized_request_is_rejected_before_queue_admission() {
        let mut request = ping();
        request.id = "x".repeat(1024 * 1024);
        let (sender, _receiver) = std::sync::mpsc::channel();
        assert!(matches!(
            ApiRequestMessage::try_new(request, sender, None),
            Err("request exceeds 1 MiB")
        ));
    }
}
