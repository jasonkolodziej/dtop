//! Opt-in tests against a real Linux Docker daemon. Each test container is labelled and removed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dtop_core::host::{resolve_hosts, HostEnvironment, TlsRequest};
use dtop_core::http::{Method, Request};
use dtop_core::json;
use dtop_core::logstream::LogStreamKind;
use dtop_core::models::ContainerState;
use dtop_core::monitoring::{build_process_tree, parse_processes, StatsSampler};
use dtop_core::tls::client_for_host;
use dtop_core::{ContainerAction, DockerApi, DockerClient, DockerError, StreamItem};

const IMAGE: &str = "alpine:3.20";

fn client() -> Option<DockerClient> {
    if std::env::var("DTOP_INTEGRATION").as_deref() != Ok("1") {
        return None;
    }
    let environment = HostEnvironment::system();
    let resolved = resolve_hosts(&[], &TlsRequest::default(), false, &environment)
        .expect("resolve the Docker host for integration tests");
    let host = resolved
        .hosts
        .first()
        .expect("integration tests need a Docker host");
    Some(
        client_for_host(host, 120)
            .expect("create the integration client")
            .negotiated()
            .expect("connect to the integration Docker daemon"),
    )
}

fn versioned_path(client: &DockerClient, path: &str) -> String {
    client
        .api_version
        .map_or_else(|| path.to_string(), |version| format!("/v{version}{path}"))
}

fn ensure_image(client: &DockerClient) {
    static READY: OnceLock<()> = OnceLock::new();
    READY.get_or_init(|| {
        let has_image = || {
            client
                .list_images()
                .expect("list Docker images")
                .iter()
                .any(|image| image.repo_tags.iter().any(|tag| tag == IMAGE))
        };
        if has_image() {
            return;
        }
        let target = Request::target(
            &versioned_path(client, "/images/create"),
            &[
                ("fromImage", Some("alpine".into())),
                ("tag", Some("3.20".into())),
            ],
        );
        let response = client
            .http
            .send(&Request::new(Method::Post, target))
            .expect("pull the integration image");
        assert!(
            response.head.is_success(),
            "pull {IMAGE}: HTTP {}: {}",
            response.status(),
            response.body_text()
        );
        assert!(
            has_image(),
            "pull {IMAGE} did not create an image: {}",
            response.body_text()
        );
    });
}

fn unique_name() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    format!(
        "dtop-rust-it-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

struct TestContainer {
    client: DockerClient,
    id: Option<String>,
    name: String,
}

impl TestContainer {
    fn start(client: &DockerClient) -> Self {
        ensure_image(client);
        let name = unique_name();
        let target = Request::target(
            &versioned_path(client, "/containers/create"),
            &[("name", Some(name.clone()))],
        );
        let mut request = Request::new(Method::Post, target);
        request.headers.set("Content-Type", "application/json");
        request.body = br#"{"Image":"alpine:3.20","Cmd":["sh","-c","echo ready; echo oops >&2; sleep 600"],"Labels":{"dtop.test":"1"}}"#.to_vec();
        let response = client.http.send(&request).expect("create test container");
        assert_eq!(
            response.status(),
            201,
            "create test container: {}",
            response.body_text()
        );
        let id = json::parse(&response.body)
            .expect("decode created container")
            .get("Id")
            .as_str()
            .expect("created container has an id")
            .to_string();
        let container = Self {
            client: client.clone(),
            id: Some(id),
            name,
        };
        client
            .perform(&ContainerAction::Start, container.id())
            .expect("start test container");
        container
    }

    fn id(&self) -> &str {
        self.id.as_deref().expect("container already removed")
    }

    fn remove(mut self) {
        self.client
            .remove_container(self.id(), true, true)
            .expect("remove test container");
        self.id = None;
    }
}

impl Drop for TestContainer {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            if let Err(error) = self.client.remove_container(&id, true, true) {
                eprintln!("failed to remove test container {id}: {error}");
            }
        }
    }
}

#[test]
fn integration_negotiates_and_reads_inventory() {
    let Some(client) = client() else { return };
    assert!(client.api_version.is_some());
    assert!(!client
        .version()
        .expect("read Docker version")
        .version
        .is_empty());
    let info = client.info().expect("read Docker info");
    assert!(info.cpu_count > 0 && info.memory_total > 0);
    client.list_containers(true).expect("list containers");
    client.list_images().expect("list images");
    client.list_networks().expect("list networks");
    client.list_volumes().expect("list volumes");
}

#[test]
fn integration_container_lifecycle_and_live_logs() {
    let Some(client) = client() else { return };
    let container = TestContainer::start(&client);
    let id = container.id().to_string();
    let listed = client
        .list_containers(true)
        .expect("list containers")
        .into_iter()
        .find(|item| item.id == id)
        .expect("test container is listed");
    assert_eq!(listed.name(), container.name);
    assert_eq!(listed.state, ContainerState::Running);

    let first = client.stats(&id).expect("read first stats");
    std::thread::sleep(Duration::from_millis(1_100));
    let second = client.stats(&id).expect("read second stats");
    let mut sampler = StatsSampler::default();
    sampler.ingest(&id, first, 0.0);
    let sample = sampler.ingest(&id, second, 1.1);
    assert!(sample.memory_usage > 0 && sample.cpu_percent.is_finite());
    let top = client.top(&id).expect("read container processes");
    assert!(!build_process_tree(&parse_processes(&top)).is_empty());
    assert!(
        client
            .inspect_container(&id)
            .expect("inspect container")
            .state
            .running
    );

    let stream = client.follow_logs(&id, 50, None);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stdout = false;
    let mut stderr = false;
    while !(stdout && stderr) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match stream.receiver.recv_timeout(remaining) {
            Ok(StreamItem::Item(lines)) => {
                for line in lines {
                    if line.stream == LogStreamKind::Stdout && line.text == "ready" {
                        stdout = line.timestamp.is_some();
                    }
                    if line.stream == LogStreamKind::Stderr && line.text == "oops" {
                        stderr = line.timestamp.is_some();
                    }
                }
            }
            other => panic!("expected timestamped stdout and stderr log lines: {other:?}"),
        }
    }
    stream.cancel();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match stream.receiver.recv_timeout(remaining) {
            Ok(StreamItem::Item(_)) => {}
            Ok(StreamItem::End) => break,
            other => panic!("cancelled log stream did not end: {other:?}"),
        }
    }

    client
        .perform(&ContainerAction::Pause, &id)
        .expect("pause container");
    assert!(
        client
            .inspect_container(&id)
            .expect("inspect paused container")
            .state
            .paused
    );
    client
        .perform(&ContainerAction::Unpause, &id)
        .expect("unpause container");
    let stop = ContainerAction::Stop {
        timeout_seconds: Some(1),
    };
    client.perform(&stop, &id).expect("stop container");
    client
        .perform(&stop, &id)
        .expect("stopping an already stopped container");
    assert_eq!(
        client
            .inspect_container(&id)
            .expect("inspect stopped container")
            .state
            .status,
        ContainerState::Exited
    );
    container.remove();
    assert!(matches!(
        client.inspect_container(&id),
        Err(DockerError::Http { status: 404, .. })
    ));
}

#[test]
fn integration_events_report_container_actions() {
    let Some(client) = client() else { return };
    let container = TestContainer::start(&client);
    let stream = client.events();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = false;
    while !seen && Instant::now() < deadline {
        client
            .perform(&ContainerAction::Pause, container.id())
            .expect("pause container");
        client
            .perform(&ContainerAction::Unpause, container.id())
            .expect("unpause container");
        match stream.receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(StreamItem::Item(event)) => {
                seen = event.kind == "container"
                    && event.actor_id == container.id()
                    && matches!(event.action.as_str(), "pause" | "unpause");
            }
            Err(RecvTimeoutError::Timeout) => {}
            other => panic!("event stream ended before a container event: {other:?}"),
        }
    }
    assert!(seen, "no event received for the test container");
    stream.cancel();
    container.remove();
}

#[test]
fn integration_missing_container_log_stream_reports_the_daemon_error() {
    let Some(client) = client() else { return };
    let stream = client.follow_logs(&unique_name(), 10, None);
    match stream.receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(StreamItem::Failed(DockerError::Http { status, message })) => {
            assert_eq!(status, 404);
            assert!(!message.is_empty());
        }
        other => panic!("expected a Docker 404 from the log stream: {other:?}"),
    }
}
