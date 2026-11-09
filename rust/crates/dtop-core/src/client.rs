//! The Docker Engine API client over a pluggable transport.

use std::sync::mpsc;
use std::sync::Arc;

use crate::api::*;
use crate::error::{DockerError, Result};
use crate::http::{percent_encode, HttpClient, Method, Request, Response, ResponseHead};
use crate::json::{self, Value};
use crate::logstream::{DecoderMode, JsonLineSplitter, LogLine, LogStreamDecoder};
use crate::models::*;
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

    /// Docker's `seconds.nanoseconds` form for `since`/`until`.
    pub fn unix_timestamp(time: f64) -> String {
        let mut seconds = time.floor() as i64;
        let mut nanos = ((time - seconds as f64) * 1_000_000_000.0).round() as i64;
        if nanos >= 1_000_000_000 {
            seconds += 1;
            nanos -= 1_000_000_000;
        }
        format!("{seconds}.{nanos:09}")
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

    fn follow_logs(&self, id: &str, tail: usize, since: Option<f64>) -> Subscription<Vec<LogLine>> {
        let mut query: Vec<(&str, Option<String>)> = vec![
            ("follow", Some("1".into())),
            ("stdout", Some("1".into())),
            ("stderr", Some("1".into())),
            ("timestamps", Some("1".into())),
        ];
        match since {
            // Resuming: everything from that instant on (inclusive; callers drop what they have).
            Some(since) => {
                query.push(("since", Some(Self::unix_timestamp(since))));
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
        // API >= 1.42 only prunes anonymous volumes unless asked; dtop keeps that safer default.
        Ok(PruneReport::decode(&self.post_json("/volumes/prune", &[])?))
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
    fn formats_since_timestamps() {
        assert_eq!(DockerClient::unix_timestamp(12.5), "12.500000000");
        assert_eq!(DockerClient::unix_timestamp(3.0), "3.000000000");
        assert_eq!(DockerClient::unix_timestamp(1.9999999999), "2.000000000");
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
}
