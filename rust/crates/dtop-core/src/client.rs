//! The Docker Engine API client over a pluggable transport.

use std::sync::mpsc;
use std::sync::Arc;

use crate::api::*;
use crate::error::{DockerError, Result};
use crate::http::{percent_encode, HttpClient, Method, Request, Response, ResponseHead};
use crate::json::{self, Value};
use crate::logstream::{DecoderMode, JsonLineSplitter, LogLine, LogStreamDecoder};
use crate::models::*;
use crate::timestamp::Timestamp;
use crate::transport::Transport;

#[derive(Clone)]
pub struct DockerClient {
    pub http: HttpClient,
    /// For long-lived streams, whose transport must not apply read timeouts.
    pub streaming_http: HttpClient,
    pub api_version: Option<ApiVersion>,
}

impl DockerClient {
    pub fn new(
        transport: Arc<dyn Transport>,
        streaming_transport: Option<Arc<dyn Transport>>,
    ) -> Self {
        let streaming = streaming_transport.unwrap_or_else(|| Arc::clone(&transport));
        DockerClient {
            http: HttpClient::new(transport),
            streaming_http: HttpClient::new(streaming),
            api_version: None,
        }
    }

    pub fn endpoint(&self) -> &crate::endpoint::DockerEndpoint {
        self.http.transport.endpoint()
    }

    /// A copy pinned to the API version negotiated with the daemon.
    pub fn negotiated(&self) -> Result<DockerClient> {
        let mut copy = self.clone();
        if let Some(server) = self.ping()? {
            copy.api_version = Some(ApiVersion::negotiate(server));
        }
        Ok(copy)
    }

    fn versioned_path(&self, path: &str) -> String {
        match self.api_version {
            Some(v) => format!("/v{v}{path}"),
            None => path.to_string(),
        }
    }

    fn request(&self, method: Method, path: &str, query: &[(&str, Option<String>)]) -> Request {
        Request::new(method, Request::target(&self.versioned_path(path), query))
    }

    /// Encodes one path value (names may contain `/` for image references). Docker accepts `:`
    /// and `@` unescaped (`sha256:…`, `image@digest`).
    pub fn path_segment(value: &str) -> String {
        value
            .split('/')
            .map(|segment| {
                percent_encode(segment)
                    .replace("%3A", ":")
                    .replace("%40", "@")
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    fn execute(&self, request: &Request, accept_not_modified: bool) -> Result<Response> {
        let response = self.http.send(request)?;
        if response.head.is_success() || (accept_not_modified && response.status() == 304) {
            Ok(response)
        } else {
            Err(error_from(&response))
        }
    }

    fn get_json(&self, path: &str, query: &[(&str, Option<String>)]) -> Result<Value> {
        let response = self.execute(&self.request(Method::Get, path, query), false)?;
        decode_body(&response)
    }

    fn post_json(&self, path: &str, query: &[(&str, Option<String>)]) -> Result<Value> {
        let response = self.execute(&self.request(Method::Post, path, query), false)?;
        decode_body(&response)
    }

    fn container_path(id: &str, suffix: &str) -> String {
        format!("/containers/{}{suffix}", Self::path_segment(id))
    }

    fn named_volume_prune_refusal(version: Option<ApiVersion>) -> DockerError {
        let daemon = version.map_or_else(
            || "This Docker daemon".to_string(),
            |v| format!("This Docker daemon (API {v})"),
        );
        DockerError::Unsupported(format!(
            "{daemon} would also delete unused named volumes when pruning, so dtop did not prune — \
             remove anonymous volumes one at a time, or upgrade to Docker Engine 23.0 (API 1.42) or newer"
        ))
    }
}

/// The daemon's `{"message": …}`, else the body text, else the reason phrase.
pub fn error_from(response: &Response) -> DockerError {
    let message = json::parse(&response.body)
        .ok()
        .and_then(|v| v.opt_str("message"))
        .unwrap_or_else(|| {
            let text = response.body_text().trim().to_string();
            if text.is_empty() {
                response.head.reason.clone()
            } else {
                text
            }
        });
    DockerError::Http {
        status: response.status(),
        message,
    }
}

fn decode_body(response: &Response) -> Result<Value> {
    json::parse(&response.body).map_err(|e| DockerError::Decoding(e.to_string()))
}

impl DockerApi for DockerClient {
    fn ping(&self) -> Result<Option<ApiVersion>> {
        let response = self.execute(&Request::new(Method::Get, "/_ping"), false)?;
        Ok(response
            .head
            .headers
            .get("API-Version")
            .and_then(ApiVersion::parse))
    }

    fn version(&self) -> Result<DockerVersion> {
        Ok(DockerVersion::decode(&self.get_json("/version", &[])?))
    }

    fn info(&self) -> Result<SystemInfo> {
        Ok(SystemInfo::decode(&self.get_json("/info", &[])?))
    }

    fn list_containers(&self, all: bool) -> Result<Vec<Container>> {
        let v = self.get_json(
            "/containers/json",
            &[("all", Some(if all { "1" } else { "0" }.into()))],
        )?;
        decode_list(&v, Container::decode)
    }

    fn inspect_container(&self, id: &str) -> Result<ContainerDetails> {
        ContainerDetails::decode(&self.get_json(&Self::container_path(id, "/json"), &[])?)
    }

    fn stats(&self, id: &str) -> Result<ContainerStats> {
        let one_shot = self
            .api_version
            .map_or(true, |v| v >= ApiVersion::ONE_SHOT_STATS);
        let query = [
            ("stream", Some("false".to_string())),
            ("one-shot", one_shot.then(|| "true".to_string())),
        ];
        let v = self.get_json(&Self::container_path(id, "/stats"), &query)?;
        if v.as_object().is_none() {
            return Err(DockerError::Decoding(
                "ContainerStats: expected an object".into(),
            ));
        }
        Ok(ContainerStats::decode(&v))
    }

    fn top(&self, id: &str) -> Result<ContainerTop> {
        Ok(ContainerTop::decode(
            &self.get_json(&Self::container_path(id, "/top"), &[])?,
        ))
    }

    fn logs(&self, id: &str, tail: usize) -> Result<Vec<LogLine>> {
        let query = [
            ("stdout", Some("1".into())),
            ("stderr", Some("1".into())),
            ("tail", Some(tail.to_string())),
        ];
        let response = self.execute(
            &self.request(Method::Get, &Self::container_path(id, "/logs"), &query),
            false,
        )?;
        let mut decoder = LogStreamDecoder::new(DecoderMode::Automatic, false);
        let mut lines = decoder.feed(&response.body);
        lines.extend(decoder.finish());
        Ok(lines)
    }

    fn follow_logs(
        &self,
        id: &str,
        tail: usize,
        since: Option<Timestamp>,
    ) -> Subscription<Vec<LogLine>> {
        let mut query: Vec<(&str, Option<String>)> = vec![
            ("follow", Some("1".into())),
            ("stdout", Some("1".into())),
            ("stderr", Some("1".into())),
            ("timestamps", Some("1".into())),
        ];
        match since {
            // Resuming: everything from that instant on (inclusive; callers drop what they have).
            Some(since) => {
                // `seconds.nanoseconds`, exact: a rounded cursor could skip or repeat lines.
                query.push(("since", Some(since.to_string())));
                query.push(("tail", Some("all".into())));
            }
            None => query.push(("tail", Some(tail.to_string()))),
        }
        let request = self.request(Method::Get, &Self::container_path(id, "/logs"), &query);
        let http = self.streaming_http.clone();
        let (tx, rx) = mpsc::channel();
        let cancellation = Arc::new(StreamCancellation::default());
        let token = Arc::clone(&cancellation);
        let spawned = std::thread::Builder::new()
            .name("dtop-logs".into())
            .spawn(move || {
                let mut decoder = LogStreamDecoder::new(DecoderMode::Automatic, true);
                let failure: std::cell::RefCell<Option<ResponseHead>> =
                    std::cell::RefCell::new(None);
                let mut error_body = Vec::new();
                let result = http.stream(
                    &request,
                    |c| token.attach(c),
                    |head| {
                        if !head.is_success() {
                            *failure.borrow_mut() = Some(head.clone());
                        }
                        Ok(())
                    },
                    |chunk| {
                        if failure.borrow().is_some() {
                            error_body.extend_from_slice(chunk);
                            return Ok(error_body.len() < 64 * 1024);
                        }
                        let lines = decoder.feed(chunk);
                        if !lines.is_empty() && tx.send(StreamItem::Item(lines)).is_err() {
                            return Ok(false);
                        }
                        Ok(!token.is_cancelled())
                    },
                );
                let last = match (result, failure.into_inner()) {
                    (Ok(()), Some(head)) => StreamItem::Failed(error_from(&Response {
                        head,
                        body: error_body,
                    })),
                    (Ok(()), None) => {
                        let rest = decoder.finish();
                        if !rest.is_empty() {
                            let _ = tx.send(StreamItem::Item(rest));
                        }
                        StreamItem::End
                    }
                    (Err(_), _) if token.is_cancelled() => StreamItem::End,
                    (Err(error), _) => StreamItem::Failed(error),
                };
                let _ = tx.send(last);
            });
        if let Err(error) = spawned {
            return Subscription::from_items(Vec::new(), Some(DockerError::Io(error.to_string())));
        }
        Subscription::new(rx, cancellation)
    }

    fn perform(&self, action: &ContainerAction, container_id: &str) -> Result<()> {
        let path = Self::container_path(container_id, &format!("/{}", action.verb()));
        // 304 means "already in that state", which is fine for an interactive tool.
        self.execute(&self.request(Method::Post, &path, &action.query()), true)
            .map(|_| ())
    }

    fn remove_container(&self, id: &str, force: bool, remove_volumes: bool) -> Result<()> {
        let query = [
            ("force", Some(flag(force))),
            ("v", Some(flag(remove_volumes))),
        ];
        self.execute(
            &self.request(Method::Delete, &Self::container_path(id, ""), &query),
            false,
        )
        .map(|_| ())
    }

    fn prune_containers(&self) -> Result<PruneReport> {
        Ok(PruneReport::decode(
            &self.post_json("/containers/prune", &[])?,
        ))
    }

    fn list_images(&self) -> Result<Vec<DockerImage>> {
        decode_list(&self.get_json("/images/json", &[])?, DockerImage::decode)
    }

    fn remove_image(&self, id: &str, force: bool) -> Result<()> {
        let path = format!("/images/{}", Self::path_segment(id));
        self.execute(
            &self.request(Method::Delete, &path, &[("force", Some(flag(force)))]),
            false,
        )
        .map(|_| ())
    }

    fn prune_images(&self, dangling_only: bool) -> Result<PruneReport> {
        let filters = if dangling_only {
            r#"{"dangling":["true"]}"#
        } else {
            r#"{"dangling":["false"]}"#
        };
        Ok(PruneReport::decode(&self.post_json(
            "/images/prune",
            &[("filters", Some(filters.into()))],
        )?))
    }

    fn list_networks(&self) -> Result<Vec<DockerNetwork>> {
        decode_list(&self.get_json("/networks", &[])?, DockerNetwork::decode)
    }

    fn remove_network(&self, id: &str) -> Result<()> {
        let path = format!("/networks/{}", Self::path_segment(id));
        self.execute(&self.request(Method::Delete, &path, &[]), false)
            .map(|_| ())
    }

    fn prune_networks(&self) -> Result<PruneReport> {
        Ok(PruneReport::decode(
            &self.post_json("/networks/prune", &[])?,
        ))
    }

    fn list_volumes(&self) -> Result<Vec<DockerVolume>> {
        let v = self.get_json("/volumes", &[])?;
        match v.get("Volumes") {
            Value::Array(_) => decode_list(v.get("Volumes"), DockerVolume::decode),
            _ => Ok(Vec::new()),
        }
    }

    fn remove_volume(&self, name: &str, force: bool) -> Result<()> {
        let path = format!("/volumes/{}", Self::path_segment(name));
        self.execute(
            &self.request(Method::Delete, &path, &[("force", Some(flag(force)))]),
            false,
        )
        .map(|_| ())
    }

    fn prune_volumes(&self) -> Result<PruneReport> {
        // Before API 1.42 a prune also deletes unused *named* volumes, and there is no filter
        // that selects anonymous ones only. dtop promises anonymous-only, so it refuses there.
        // An unnegotiated client asks the daemon which version it speaks first.
        let version = match self.api_version {
            Some(v) => Some(v),
            None => self.ping()?,
        };
        if let Some(v) = version.filter(|v| *v < ApiVersion::ANONYMOUS_VOLUME_PRUNE) {
            return Err(Self::named_volume_prune_refusal(Some(v)));
        }
        // `all=false` states the 1.42+ default explicitly. A daemon too old to know the filter
        // (one whose version was not reported) rejects the request instead of deleting named
        // volumes.
        let query = [("filters", Some(r#"{"all":["false"]}"#.to_string()))];
        match self.post_json("/volumes/prune", &query) {
            Ok(v) => Ok(PruneReport::decode(&v)),
            Err(DockerError::Http {
                status: 400,
                message,
            }) if message.to_ascii_lowercase().contains("invalid filter") => {
                Err(Self::named_volume_prune_refusal(None))
            }
            Err(e) => Err(e),
        }
    }

    fn events(&self) -> Subscription<DockerEvent> {
        let filters = r#"{"type":["container","image","network","volume"]}"#;
        let request = self.request(Method::Get, "/events", &[("filters", Some(filters.into()))]);
        json_stream(request, self.streaming_http.clone(), DockerEvent::decode)
    }
}

fn flag(value: bool) -> String {
    if value { "1" } else { "0" }.to_string()
}

/// Streams newline-delimited JSON records on a background thread until cancelled.
pub fn json_stream<T: Send + 'static>(
    request: Request,
    http: HttpClient,
    decode: fn(&Value) -> T,
) -> Subscription<T> {
    let (tx, rx) = mpsc::channel();
    let cancellation = Arc::new(StreamCancellation::default());
    let token = Arc::clone(&cancellation);
    let spawned = std::thread::Builder::new()
        .name("dtop-events".into())
        .spawn(move || {
            let mut splitter = JsonLineSplitter::default();
            let failure: std::cell::RefCell<Option<DockerError>> = std::cell::RefCell::new(None);
            let result = http.stream(
                &request,
                |c| token.attach(c),
                |head| {
                    if !head.is_success() {
                        *failure.borrow_mut() = Some(DockerError::Http {
                            status: head.status,
                            message: head.reason.clone(),
                        });
                    }
                    Ok(())
                },
                |chunk| {
                    if failure.borrow().is_some() {
                        return Ok(false);
                    }
                    for line in splitter.feed(chunk)? {
                        if let Ok(value) = json::parse(&line) {
                            if tx.send(StreamItem::Item(decode(&value))).is_err() {
                                return Ok(false);
                            }
                        }
                    }
                    Ok(!token.is_cancelled())
                },
            );
            let last = match (result, failure.into_inner()) {
                (_, Some(error)) => StreamItem::Failed(error),
                (Ok(()), None) => StreamItem::End,
                (Err(_), None) if token.is_cancelled() => StreamItem::End,
                (Err(error), None) => StreamItem::Failed(error),
            };
            let _ = tx.send(last);
        });
    if let Err(error) = spawned {
        return Subscription::from_items(Vec::new(), Some(DockerError::Io(error.to_string())));
    }
    Subscription::new(rx, cancellation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_path_segments_like_docker() {
        assert_eq!(DockerClient::path_segment("sha256:abc"), "sha256:abc");
        assert_eq!(
            DockerClient::path_segment("repo/img@sha256:x"),
            "repo/img@sha256:x"
        );
        assert_eq!(DockerClient::path_segment("a b"), "a%20b");
    }

    #[test]
    fn formats_since_timestamps_exactly() {
        let at = |seconds, nanos| Timestamp { seconds, nanos };
        assert_eq!(at(12, 500_000_000).to_string(), "12.500000000");
        assert_eq!(at(3, 0).to_string(), "3.000000000");
        assert_eq!(
            at(1_714_566_896, 123_456_789).to_string(),
            "1714566896.123456789"
        );
    }

    #[test]
    fn extracts_error_messages() {
        let head = ResponseHead {
            status: 404,
            reason: "Not Found".into(),
            headers: Default::default(),
        };
        let r = Response {
            head: head.clone(),
            body: br#"{"message":"No such container: x"}"#.to_vec(),
        };
        assert_eq!(
            error_from(&r),
            DockerError::Http {
                status: 404,
                message: "No such container: x".into()
            }
        );
        let r = Response {
            head,
            body: Vec::new(),
        };
        assert_eq!(
            error_from(&r),
            DockerError::Http {
                status: 404,
                message: "Not Found".into()
            }
        );
    }

    /// Status, body and extra headers of one scripted reply.
    type Reply = (u16, &'static str, Vec<(&'static str, &'static str)>);

    /// Serves one scripted response per connection and records each request line.
    fn scripted_server(responses: Vec<Reply>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body, headers) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap() == 1 {
                    head.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                requests.push(text.lines().next().unwrap_or_default().to_string());
                let mut reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    reply.push_str(&format!("{name}: {value}\r\n"));
                }
                reply.push_str("Connection: close\r\n\r\n");
                reply.push_str(body);
                stream.write_all(reply.as_bytes()).unwrap();
            }
            requests
        });
        (port, handle)
    }

    fn client(port: u16) -> DockerClient {
        let transport = Arc::new(crate::transport::TcpTransport::new("127.0.0.1", port, 5));
        DockerClient::new(transport, None)
    }

    #[test]
    fn volume_prune_is_refused_before_api_1_42() {
        // Negotiated to an old version: refused without a request.
        let mut old = client(1);
        old.api_version = ApiVersion::parse("1.41");
        assert!(matches!(
            old.prune_volumes(),
            Err(DockerError::Unsupported(_))
        ));

        // Unnegotiated: the ping reports 1.41, so nothing is pruned.
        let (port, server) = scripted_server(vec![(200, "OK", vec![("API-Version", "1.41")])]);
        let error = client(port).prune_volumes().unwrap_err();
        assert!(error.to_string().contains("named volumes"), "{error}");
        assert_eq!(server.join().unwrap(), vec!["GET /_ping HTTP/1.1"]);
    }

    #[test]
    fn volume_prune_selects_anonymous_volumes_explicitly() {
        let (port, server) = scripted_server(vec![
            (200, "OK", vec![("API-Version", "1.47")]),
            (
                200,
                r#"{"VolumesDeleted":["a"],"SpaceReclaimed":5}"#,
                vec![],
            ),
        ]);
        let report = client(port).prune_volumes().unwrap();
        assert_eq!(report.space_reclaimed, 5);
        let requests = server.join().unwrap();
        assert_eq!(
            requests[1],
            "POST /volumes/prune?filters=%7B%22all%22%3A%5B%22false%22%5D%7D HTTP/1.1"
        );

        // A daemon that reports no version and does not know the filter: refused, not pruned.
        let (port, server) = scripted_server(vec![
            (200, "OK", vec![]),
            (400, r#"{"message":"Invalid filter 'all'"}"#, vec![]),
        ]);
        let error = client(port).prune_volumes().unwrap_err();
        assert!(matches!(error, DockerError::Unsupported(_)), "{error}");
        server.join().unwrap();
    }
}
