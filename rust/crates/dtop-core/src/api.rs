//! What dtop needs from the Docker Engine. `DockerClient` is the real implementation; tests use
//! fakes. Calls block; streams run on their own thread and deliver through a channel.

use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::error::{DockerError, Result};
use crate::logstream::LogLine;
use crate::models::*;
use crate::transport::Connection;

/// Lifecycle operations on one container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerAction {
    Start,
    Stop { timeout_seconds: Option<i64> },
    Restart { timeout_seconds: Option<i64> },
    Kill { signal: Option<String> },
    Pause,
    Unpause,
}

impl ContainerAction {
    pub fn verb(&self) -> &'static str {
        match self {
            ContainerAction::Start => "start",
            ContainerAction::Stop { .. } => "stop",
            ContainerAction::Restart { .. } => "restart",
            ContainerAction::Kill { .. } => "kill",
            ContainerAction::Pause => "pause",
            ContainerAction::Unpause => "unpause",
        }
    }

    pub fn query(&self) -> Vec<(&'static str, Option<String>)> {
        match self {
            ContainerAction::Stop { timeout_seconds }
            | ContainerAction::Restart { timeout_seconds } => {
                vec![("t", timeout_seconds.map(|t| t.to_string()))]
            }
            ContainerAction::Kill { signal } => vec![("signal", signal.clone())],
            _ => Vec::new(),
        }
    }
}

/// A Docker API version such as `1.43`, ordered numerically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiVersion {
    pub major: u32,
    pub minor: u32,
}

impl ApiVersion {
    /// Newest API version dtop has been written against.
    pub const MAXIMUM_SUPPORTED: ApiVersion = ApiVersion {
        major: 1,
        minor: 47,
    };
    /// `volumes/prune` only removes anonymous volumes (and accepts the `all` filter) from 1.42.
    pub const ANONYMOUS_VOLUME_PRUNE: ApiVersion = ApiVersion {
        major: 1,
        minor: 42,
    };
    /// `stats?one-shot=true` arrived in 1.41.
    pub const ONE_SHOT_STATS: ApiVersion = ApiVersion {
        major: 1,
        minor: 41,
    };

    pub fn parse(text: &str) -> Option<ApiVersion> {
        let mut parts = text.trim().split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        parts
            .next()
            .is_none()
            .then_some(ApiVersion { major, minor })
    }

    /// The version to speak: the daemon's, capped at what dtop understands.
    pub fn negotiate(server: ApiVersion) -> ApiVersion {
        server.min(Self::MAXIMUM_SUPPORTED)
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// One message from a stream.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamItem<T> {
    Item(T),
    /// The stream ended normally (or was cancelled).
    End,
    Failed(DockerError),
}

/// Holds the live connection of a streaming request so it can be interrupted from elsewhere.
#[derive(Default)]
pub struct StreamCancellation {
    state: Mutex<(Option<Arc<dyn Connection>>, bool)>,
}

impl StreamCancellation {
    pub fn is_cancelled(&self) -> bool {
        self.state.lock().map(|s| s.1).unwrap_or(true)
    }

    pub fn attach(&self, connection: Arc<dyn Connection>) {
        let should_interrupt = match self.state.lock() {
            Ok(mut state) => {
                state.0 = Some(Arc::clone(&connection));
                state.1
            }
            Err(_) => true,
        };
        if should_interrupt {
            connection.interrupt();
        }
    }

    pub fn cancel(&self) {
        let connection = match self.state.lock() {
            Ok(mut state) => {
                state.1 = true;
                state.0.clone()
            }
            Err(_) => None,
        };
        if let Some(connection) = connection {
            connection.interrupt();
        }
    }
}

/// A running stream. Dropping it (or calling `cancel`) interrupts the connection.
pub struct Subscription<T> {
    pub receiver: mpsc::Receiver<StreamItem<T>>,
    cancellation: Arc<StreamCancellation>,
}

impl<T> Subscription<T> {
    pub fn new(
        receiver: mpsc::Receiver<StreamItem<T>>,
        cancellation: Arc<StreamCancellation>,
    ) -> Self {
        Subscription {
            receiver,
            cancellation,
        }
    }

    /// A finished stream that yields `items` and then `End` (or `Failed`).
    pub fn from_items(items: Vec<T>, error: Option<DockerError>) -> Self {
        let (tx, rx) = mpsc::channel();
        for item in items {
            let _ = tx.send(StreamItem::Item(item));
        }
        let _ = tx.send(error.map_or(StreamItem::End, StreamItem::Failed));
        Subscription::new(rx, Arc::new(StreamCancellation::default()))
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn cancellation(&self) -> Arc<StreamCancellation> {
        Arc::clone(&self.cancellation)
    }
}

impl<T> Drop for Subscription<T> {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Everything dtop needs from a Docker daemon.
pub trait DockerApi: Send + Sync {
    fn ping(&self) -> Result<Option<ApiVersion>>;
    fn version(&self) -> Result<DockerVersion>;
    fn info(&self) -> Result<SystemInfo>;

    fn list_containers(&self, all: bool) -> Result<Vec<Container>>;
    fn inspect_container(&self, id: &str) -> Result<ContainerDetails>;
    fn stats(&self, id: &str) -> Result<ContainerStats>;
    fn top(&self, id: &str) -> Result<ContainerTop>;
    fn logs(&self, id: &str, tail: usize) -> Result<Vec<LogLine>>;
    /// The last `tail` lines (or, with `since`, every line from that time on), then every new
    /// line as the container writes it (`logs -f`). Lines carry timestamps. Ends when the
    /// container stops or the subscription is cancelled. The default fetches once.
    fn follow_logs(
        &self,
        id: &str,
        tail: usize,
        since: Option<crate::timestamp::Timestamp>,
    ) -> Subscription<Vec<LogLine>> {
        let _ = since;
        match self.logs(id, tail) {
            Ok(lines) => Subscription::from_items(vec![lines], None),
            Err(error) => Subscription::from_items(Vec::new(), Some(error)),
        }
    }
    fn perform(&self, action: &ContainerAction, container_id: &str) -> Result<()>;
    fn remove_container(&self, id: &str, force: bool, remove_volumes: bool) -> Result<()>;
    fn prune_containers(&self) -> Result<PruneReport>;

    fn list_images(&self) -> Result<Vec<DockerImage>>;
    fn remove_image(&self, id: &str, force: bool) -> Result<()>;
    fn prune_images(&self, dangling_only: bool) -> Result<PruneReport>;

    fn list_networks(&self) -> Result<Vec<DockerNetwork>>;
    fn remove_network(&self, id: &str) -> Result<()>;
    fn prune_networks(&self) -> Result<PruneReport>;

    fn list_volumes(&self) -> Result<Vec<DockerVolume>>;
    fn remove_volume(&self, name: &str, force: bool) -> Result<()>;
    fn prune_volumes(&self) -> Result<PruneReport>;

    fn events(&self) -> Subscription<DockerEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_and_orders_versions() {
        let v = ApiVersion::parse("1.45").unwrap();
        assert!(v < ApiVersion::parse("1.46").unwrap());
        assert_eq!(
            ApiVersion::negotiate(ApiVersion::parse("1.99").unwrap()),
            ApiVersion::MAXIMUM_SUPPORTED
        );
        assert_eq!(ApiVersion::parse("1"), None);
        assert_eq!(ApiVersion::parse("1.2.3"), None);
        assert_eq!(v.to_string(), "1.45");
    }

    #[test]
    fn action_queries() {
        assert_eq!(
            ContainerAction::Stop {
                timeout_seconds: Some(5)
            }
            .query(),
            vec![("t", Some("5".to_string()))]
        );
        assert_eq!(ContainerAction::Kill { signal: None }.verb(), "kill");
    }
}
