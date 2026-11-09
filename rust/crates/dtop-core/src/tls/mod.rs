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
use crate::transport::{make_transport, Connection, Transport};

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
    pub timeout_seconds: u64,
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
        }
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
        self.connect_tls().map(|(c, _)| c)
    }
}

/// The transport for a host: TLS when it has TLS settings, otherwise its plain transport.
pub fn make_host_transport(host: &DockerHost, timeout_seconds: u64) -> Result<Arc<dyn Transport>> {
    if let (Some(tls), DockerEndpoint::Tcp { host: name, port }) = (&host.tls, &host.endpoint) {
        return Ok(Arc::new(TlsTransport::new(
            name,
            *port,
            tls.clone(),
            timeout_seconds,
        )));
    }
    make_transport(&host.endpoint, timeout_seconds)
}

/// A client for a host (TLS when the host asks for it), with a timeout-free streaming transport.
pub fn client_for_host(host: &DockerHost, timeout_seconds: u64) -> Result<DockerClient> {
    let transport = make_host_transport(host, timeout_seconds)?;
    let streaming = make_host_transport(host, 0)?;
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
