//! TLS to Docker daemons (`--tlsverify`, `DOCKER_TLS_VERIFY`, `https://`) on each platform's own
//! stack (ADR 0003): OpenSSL loaded at runtime on Linux, Network.framework on macOS, SChannel on
//! Windows. Everything above `Connection` is TLS-agnostic.

#[cfg(target_os = "macos")]
mod apple;
#[cfg(all(unix, not(target_os = "macos")))]
mod openssl;
pub mod pem;
#[cfg(windows)]
mod schannel;

use std::sync::Arc;

use crate::client::DockerClient;
use crate::endpoint::DockerEndpoint;
#[cfg(any(target_os = "macos", windows))]
use crate::error::DockerError;
use crate::error::Result;
use crate::host::{DockerHost, TlsConfiguration};
use crate::transport::{make_streaming_transport, make_transport, Connection, Transport};

/// A negotiated protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsProtocolVersion {
    Tls12,
    Tls13,
}

impl TlsProtocolVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            TlsProtocolVersion::Tls12 => "TLS 1.2",
            TlsProtocolVersion::Tls13 => "TLS 1.3",
        }
    }
}

/// TCP + TLS to one daemon.
pub struct TlsTransport {
    endpoint: DockerEndpoint,
    pub host: String,
    pub port: u16,
    pub configuration: TlsConfiguration,
    /// Bounds the TCP connect and the handshake, and (unless `streaming`) every later read.
    pub timeout_seconds: u64,
    /// For long-lived streams: once the handshake is done, reads wait indefinitely.
    pub streaming: bool,
}

impl TlsTransport {
    pub fn new(
        host: &str,
        port: u16,
        configuration: TlsConfiguration,
        timeout_seconds: u64,
    ) -> Self {
        TlsTransport {
            endpoint: DockerEndpoint::Tcp {
                host: host.into(),
                port,
            },
            host: host.into(),
            port,
            configuration,
            timeout_seconds,
            streaming: false,
        }
    }

    /// For long-lived streams: setup keeps the timeout, reads afterwards wait indefinitely.
    pub fn streaming(mut self) -> Self {
        self.streaming = true;
        self
    }

    /// The name the server certificate must match.
    pub fn server_name(&self) -> String {
        self.configuration
            .server_name
            .clone()
            .unwrap_or_else(|| self.host.clone())
    }

    /// Connects and reports the negotiated protocol version (used by tests and `--once`).
    pub fn connect_tls(&self) -> Result<(Arc<dyn Connection>, Option<TlsProtocolVersion>)> {
        backend::connect(self)
    }
}

impl Transport for TlsTransport {
    fn endpoint(&self) -> &DockerEndpoint {
        &self.endpoint
    }

    fn connect(&self) -> Result<Arc<dyn Connection>> {
        let (connection, _) = self.connect_tls()?;
        if self.streaming {
            if let Err(error) = connection.disable_read_timeout() {
                connection.close();
                return Err(error);
            }
        }
        Ok(connection)
    }
}

/// The transport for a host: TLS when it has TLS settings, otherwise its plain transport.
pub fn make_host_transport(host: &DockerHost, timeout_seconds: u64) -> Result<Arc<dyn Transport>> {
    host_transport(host, timeout_seconds, false)
}

/// The transport for a host's long-lived streams: connecting and the TLS handshake still time
/// out after `setup_timeout_seconds` (a stalled peer cannot block the worker forever, and the
/// stream's cancellation only takes effect once connected); then reads wait indefinitely.
pub fn make_streaming_host_transport(
    host: &DockerHost,
    setup_timeout_seconds: u64,
) -> Result<Arc<dyn Transport>> {
    host_transport(host, setup_timeout_seconds, true)
}

fn host_transport(
    host: &DockerHost,
    timeout_seconds: u64,
    streaming: bool,
) -> Result<Arc<dyn Transport>> {
    if let (Some(tls), DockerEndpoint::Tcp { host: name, port }) = (&host.tls, &host.endpoint) {
        let transport = TlsTransport::new(name, *port, tls.clone(), timeout_seconds);
        return Ok(Arc::new(if streaming {
            transport.streaming()
        } else {
            transport
        }));
    }
    if streaming {
        make_streaming_transport(&host.endpoint, timeout_seconds)
    } else {
        make_transport(&host.endpoint, timeout_seconds)
    }
}

/// A client for a host (TLS when the host asks for it). Its streaming transport keeps
/// `timeout_seconds` for connecting and the handshake, and drops the read timeout afterwards.
pub fn client_for_host(host: &DockerHost, timeout_seconds: u64) -> Result<DockerClient> {
    let transport = make_host_transport(host, timeout_seconds)?;
    let streaming = make_streaming_host_transport(host, timeout_seconds)?;
    Ok(DockerClient::new(transport, Some(streaming)))
}

/// Forces SChannel's legacy credential path for the current thread during a test.
#[cfg(all(windows, feature = "tls-test-support"))]
#[doc(hidden)]
pub fn with_legacy_schannel_for_test<T>(test: impl FnOnce() -> T) -> T {
    schannel::with_legacy_credentials(test)
}

/// `DTOP_TLS_TRACE=1` logs TLS internals to stderr.
pub fn trace(message: impl FnOnce() -> String) {
    if std::env::var("DTOP_TLS_TRACE").as_deref() == Ok("1") {
        eprintln!("dtop tls: {}", message());
    }
}

/// Whether `name` is an IPv4 or IPv6 literal (certificates match those against IP SANs).
pub fn is_ip_address(name: &str) -> bool {
    if name.contains(':') {
        let address = name.split('%').next().unwrap_or(name);
        return !address.is_empty()
            && address
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
    }
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.chars().all(|c| c.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|v| v <= 255)
        })
}

pub fn closed_during_handshake_hint(configuration: &TlsConfiguration) -> String {
    if !configuration.has_client_certificate() {
        return "the daemon probably requires a client certificate: pass --tlscert and --tlskey, \
                or put cert.pem and key.pem in ~/.docker (or DOCKER_CERT_PATH)"
            .into();
    }
    "check that the port serves TLS and that the daemon trusts your client certificate".into()
}

pub fn name_mismatch_message(server_name: &str, endpoint: &str) -> String {
    format!(
        "the certificate of {endpoint} is not valid for '{server_name}' — connect with a name listed in the \
         certificate, or reissue the daemon certificate with '{server_name}' as a subjectAltName"
    )
}

pub fn untrusted_message(reason: &str, endpoint: &str, configuration: &TlsConfiguration) -> String {
    let mut message = format!("could not verify the certificate of {endpoint} ({reason}) — ");
    match &configuration.ca_file {
        Some(ca) => message.push_str(&format!(
            "{ca} did not issue it; check --tlscacert or DOCKER_CERT_PATH"
        )),
        None => message
            .push_str("pass the CA that issued it with --tlscacert, or put it in ~/.docker/ca.pem"),
    }
    message
}

pub fn handshake_message(detail: &str, endpoint: &str, configuration: &TlsConfiguration) -> String {
    let lowered = detail.to_lowercase();
    if [
        "wrong version number",
        "packet length too long",
        "http request",
        "record layer failure",
    ]
    .iter()
    .any(|s| lowered.contains(s))
    {
        return format!(
            "{endpoint} does not speak TLS on this port ({detail}) — plain-HTTP daemons usually listen on 2375; \
             drop --tls/--tlsverify for them"
        );
    }
    if lowered.contains("certificate required")
        || (!configuration.has_client_certificate() && lowered.contains("handshake failure"))
    {
        return format!(
            "{endpoint} requires a client certificate ({detail}) — {}",
            closed_during_handshake_hint(configuration)
        );
    }
    if [
        "unknown ca",
        "bad certificate",
        "certificate unknown",
        "decrypt error",
    ]
    .iter()
    .any(|s| lowered.contains(s))
    {
        return format!(
            "{endpoint} rejected the client certificate ({detail}) — it must be issued by the CA the daemon trusts \
             (dockerd --tlscacert)"
        );
    }
    let mut message = format!("TLS handshake with {endpoint} failed: {detail}");
    if !configuration.has_client_certificate() {
        message.push_str(
            " — if the daemon requires a client certificate, pass --tlscert and --tlskey, \
             or put cert.pem and key.pem in ~/.docker (or DOCKER_CERT_PATH)",
        );
    }
    message
}

#[cfg(any(target_os = "macos", windows))]
pub(crate) fn read_file(path: &str, what: &str) -> Result<String> {
    std::fs::read_to_string(path)
        .map_err(|e| DockerError::Tls(format!("could not read the {what} {path}: {e}")))
}

mod backend {
    use super::*;

    #[cfg(target_os = "macos")]
    pub fn connect(t: &TlsTransport) -> Result<(Arc<dyn Connection>, Option<TlsProtocolVersion>)> {
        let endpoint = t.endpoint.to_string();
        let connection = apple::AppleTlsConnection::connect(
            &t.host,
            t.port,
            &t.configuration,
            &t.server_name(),
            &endpoint,
            t.timeout_seconds,
        )?;
        let negotiated = connection.negotiated;
        Ok((Arc::new(connection), negotiated))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn connect(t: &TlsTransport) -> Result<(Arc<dyn Connection>, Option<TlsProtocolVersion>)> {
        let endpoint = t.endpoint.to_string();
        let stream =
            crate::transport::connect_tcp(&t.host, t.port, t.timeout_seconds, &t.endpoint)?;
        let raw: Arc<dyn Connection> = Arc::new(crate::transport::StreamConnection::new(stream));
        let connection = openssl::OpenSslConnection::connect(
            raw,
            &t.configuration,
            &t.server_name(),
            &endpoint,
        )?;
        let negotiated = connection.negotiated;
        Ok((Arc::new(connection), negotiated))
    }

    #[cfg(windows)]
    pub fn connect(t: &TlsTransport) -> Result<(Arc<dyn Connection>, Option<TlsProtocolVersion>)> {
        let endpoint = t.endpoint.to_string();
        let stream =
            crate::transport::connect_tcp(&t.host, t.port, t.timeout_seconds, &t.endpoint)?;
        let raw: Arc<dyn Connection> = Arc::new(crate::transport::StreamConnection::new(stream));
        let connection = schannel::SChannelConnection::connect(
            raw,
            &t.configuration,
            &t.server_name(),
            &endpoint,
        )?;
        let negotiated = connection.negotiated;
        Ok((Arc::new(connection), negotiated))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// A peer that accepts one TCP connection and then says nothing until the returned sender
    /// is dropped.
    fn silent_peer() -> (u16, mpsc::Sender<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, stopped) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = stopped.recv_timeout(Duration::from_secs(30));
            drop(stream);
        });
        (port, stop)
    }

    fn tls_host(port: u16) -> DockerHost {
        let mut host = DockerHost::new(
            "stalled",
            DockerEndpoint::Tcp {
                host: "127.0.0.1".into(),
                port,
            },
        );
        host.tls = Some(TlsConfiguration::default());
        host
    }

    #[test]
    fn streaming_transports_keep_a_deadline_for_the_tls_handshake() {
        let (port, peer) = silent_peer();
        let transport = make_streaming_host_transport(&tls_host(port), 1).unwrap();
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        std::thread::spawn(move || {
            let _ = tx.send(transport.connect().map(|_| ()));
        });
        // The handshake stalls; it must fail on the setup deadline (Network.framework adds a
        // grace period), not wait for the peer.
        let outcome = rx
            .recv_timeout(Duration::from_secs(7))
            .expect("the handshake blocked past its deadline");
        // Without a TLS library the connect fails earlier, which is fine for this check.
        assert!(outcome.is_err());
        assert!(started.elapsed() < Duration::from_secs(7));
        drop(peer);
    }

    #[test]
    fn streaming_transports_wait_indefinitely_once_connected() {
        let (port, peer) = silent_peer();
        let endpoint = DockerEndpoint::Tcp {
            host: "127.0.0.1".into(),
            port,
        };
        let host = DockerHost::new("plain", endpoint);
        let connection = make_streaming_host_transport(&host, 1)
            .unwrap()
            .connect()
            .unwrap();
        let reader = Arc::clone(&connection);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(reader.read(16));
        });
        // Well past the one-second setup timeout, the read is still waiting…
        assert!(rx.recv_timeout(Duration::from_millis(2500)).is_err());
        // …until the stream is cancelled.
        connection.interrupt();
        let read = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(read, Ok(ref b) if b.is_empty()) || read.is_err());

        // A request transport keeps its read timeout.
        let (port, peer2) = silent_peer();
        let host = DockerHost::new(
            "plain",
            DockerEndpoint::Tcp {
                host: "127.0.0.1".into(),
                port,
            },
        );
        let connection = make_host_transport(&host, 1).unwrap().connect().unwrap();
        assert!(matches!(
            connection.read(16),
            Err(crate::error::DockerError::TimedOut(_))
        ));
        drop((peer, peer2));
    }

    #[test]
    fn recognizes_ip_literals() {
        assert!(is_ip_address("10.0.0.1"));
        assert!(is_ip_address("::1"));
        assert!(is_ip_address("fe80::1%en0"));
        assert!(!is_ip_address("docker.example"));
        assert!(!is_ip_address("256.1.1.1"));
    }

    #[test]
    fn handshake_messages_say_what_to_change() {
        let config = TlsConfiguration {
            verify_peer: true,
            ..Default::default()
        };
        assert!(
            handshake_message("wrong version number", "tcp://h:2375", &config)
                .contains("does not speak TLS")
        );
        assert!(
            handshake_message("sslv3 alert handshake failure", "e", &config)
                .contains("requires a client certificate")
        );
        let with_cert = TlsConfiguration {
            certificate_file: Some("c".into()),
            key_file: Some("k".into()),
            ..config
        };
        assert!(handshake_message("tlsv1 alert unknown ca", "e", &with_cert)
            .contains("rejected the client certificate"));
    }
}
