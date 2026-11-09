//! Where the Docker Engine API lives, and `DOCKER_HOST` parsing.

use std::fmt;

use crate::error::{DockerError, Result};
use crate::platform::Platform;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DockerEndpoint {
    UnixSocket(String),
    NamedPipe(String),
    Tcp { host: String, port: u16 },
}

impl DockerEndpoint {
    pub const DEFAULT_TCP_PORT: u16 = 2375;
    /// Docker's conventional port for TLS (`--tlsverify`) daemons.
    pub const DEFAULT_TLS_PORT: u16 = 2376;
    pub const DEFAULT_WINDOWS_PIPE: &'static str = r"\\.\pipe\docker_engine";
    pub const DEFAULT_UNIX_SOCKET: &'static str = "/var/run/docker.sock";

    /// Value sent in the HTTP `Host` header. Docker ignores it for local transports but HTTP/1.1
    /// requires one.
    pub fn host_header(&self) -> String {
        match self {
            DockerEndpoint::Tcp { host, port } => format!("{}:{port}", bracket(host)),
            _ => "docker".to_string(),
        }
    }

    /// Whether the daemon runs on this machine (sockets, pipes, loopback TCP), so that things
    /// measured here — like GPU usage — belong to it.
    pub fn is_local(&self) -> bool {
        match self {
            DockerEndpoint::UnixSocket(_) | DockerEndpoint::NamedPipe(_) => true,
            DockerEndpoint::Tcp { host, .. } => {
                matches!(
                    host.to_lowercase().as_str(),
                    "localhost" | "127.0.0.1" | "::1"
                )
            }
        }
    }

    /// The host name or address of a TCP endpoint.
    pub fn tcp_host(&self) -> Option<&str> {
        match self {
            DockerEndpoint::Tcp { host, .. } => Some(host),
            _ => None,
        }
    }

    pub fn is_tcp(&self) -> bool {
        matches!(self, DockerEndpoint::Tcp { .. })
    }

    /// Whether `raw` names an endpoint that is TLS by its scheme (`https://`).
    pub fn scheme_implies_tls(raw: &str) -> bool {
        raw.trim().to_lowercase().starts_with("https://")
    }

    /// Parses `DOCKER_HOST`-style strings. A TCP address without a port gets 2376 when `tls` is
    /// true (or the scheme is `https://`) and 2375 otherwise, as the Docker CLI does.
    pub fn parse(raw: &str, tls: bool) -> Result<DockerEndpoint> {
        let value = raw.trim();
        let invalid = || DockerError::InvalidEndpoint(raw.to_string());
        if value.is_empty() {
            return Err(invalid());
        }
        if let Some(rest) = value.strip_prefix("unix://") {
            if rest.is_empty() {
                return Err(invalid());
            }
            return Ok(DockerEndpoint::UnixSocket(rest.to_string()));
        }
        if let Some(rest) = value.strip_prefix("npipe://") {
            // npipe:////./pipe/docker_engine -> \\.\pipe\docker_engine
            let normalized = rest.replace('/', "\\");
            if !normalized.starts_with(r"\\") || normalized.chars().count() <= 2 {
                return Err(invalid());
            }
            return Ok(DockerEndpoint::NamedPipe(normalized));
        }
        if let Some(rest) = value
            .strip_prefix("tcp://")
            .or_else(|| value.strip_prefix("http://"))
        {
            let port = if tls {
                Self::DEFAULT_TLS_PORT
            } else {
                Self::DEFAULT_TCP_PORT
            };
            return parse_host_port(rest, raw, port);
        }
        if let Some(rest) = value.strip_prefix("https://") {
            return parse_host_port(rest, raw, Self::DEFAULT_TLS_PORT);
        }
        if value.starts_with("ssh://") {
            return Err(DockerError::UnsupportedEndpoint(raw.to_string()));
        }
        if value.contains("://") {
            return Err(invalid());
        }
        // Bare absolute paths are accepted as a convenience.
        if value.starts_with('/') {
            return Ok(DockerEndpoint::UnixSocket(value.to_string()));
        }
        if value.starts_with(r"\\") {
            return Ok(DockerEndpoint::NamedPipe(value.to_string()));
        }
        Err(invalid())
    }

    /// Picks the endpoint to use: an explicit override, then `DOCKER_HOST`, then the first
    /// well-known socket that exists for the platform.
    pub fn resolve(
        override_value: Option<&str>,
        environment: &dyn Fn(&str) -> Option<String>,
        platform: Platform,
        home_directory: Option<&str>,
        file_exists: &dyn Fn(&str) -> bool,
    ) -> Result<DockerEndpoint> {
        if let Some(value) = override_value.filter(|v| !v.trim().is_empty()) {
            return Self::parse(value, false);
        }
        if let Some(host) = environment("DOCKER_HOST").filter(|v| !v.trim().is_empty()) {
            return Self::parse(&host, false);
        }
        if platform == Platform::Windows {
            return Ok(DockerEndpoint::NamedPipe(
                Self::DEFAULT_WINDOWS_PIPE.to_string(),
            ));
        }
        let candidates = Self::candidate_sockets(environment, platform, home_directory);
        let socket = candidates
            .into_iter()
            .find(|path| file_exists(path))
            .unwrap_or_else(|| Self::DEFAULT_UNIX_SOCKET.to_string());
        Ok(DockerEndpoint::UnixSocket(socket))
    }

    /// Well-known socket locations in priority order.
    pub fn candidate_sockets(
        environment: &dyn Fn(&str) -> Option<String>,
        platform: Platform,
        home_directory: Option<&str>,
    ) -> Vec<String> {
        let mut candidates = vec![Self::DEFAULT_UNIX_SOCKET.to_string()];
        match platform {
            Platform::MacOS => {
                if let Some(home) = home_directory {
                    for suffix in [
                        "/.docker/run/docker.sock",
                        "/.orbstack/run/docker.sock",
                        "/.colima/default/docker.sock",
                        "/.colima/docker.sock",
                        "/.rd/docker.sock",
                    ] {
                        candidates.push(format!("{home}{suffix}"));
                    }
                }
            }
            Platform::Linux | Platform::Other => {
                if let Some(runtime) = environment("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
                    candidates.push(format!("{runtime}/docker.sock"));
                }
                if let Some(home) = home_directory {
                    candidates.push(format!("{home}/.docker/desktop/docker.sock"));
                }
            }
            Platform::Windows => return Vec::new(),
        }
        candidates
    }
}

fn bracket(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

impl fmt::Display for DockerEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DockerEndpoint::UnixSocket(path) => write!(f, "unix://{path}"),
            DockerEndpoint::NamedPipe(path) => write!(f, "npipe://{}", path.replace('\\', "/")),
            DockerEndpoint::Tcp { host, port } => write!(f, "tcp://{}:{port}", bracket(host)),
        }
    }
}

fn parse_host_port(rest: &str, original: &str, default_port: u16) -> Result<DockerEndpoint> {
    let invalid = || DockerError::InvalidEndpoint(original.to_string());
    let mut authority = rest;
    if let Some(slash) = authority.find('/') {
        if &authority[slash..] != "/" {
            return Err(invalid());
        }
        authority = &authority[..slash];
    }
    if authority.is_empty() {
        return Err(invalid());
    }
    let (mut host, port_text): (String, Option<&str>) =
        if let Some(inner) = authority.strip_prefix('[') {
            let close = inner.find(']').ok_or_else(invalid)?;
            let after = &inner[close + 1..];
            let port = if after.is_empty() {
                None
            } else {
                Some(after.strip_prefix(':').ok_or_else(invalid)?)
            };
            (inner[..close].to_string(), port)
        } else if let Some(colon) = authority.rfind(':') {
            (
                authority[..colon].to_string(),
                Some(&authority[colon + 1..]),
            )
        } else {
            (authority.to_string(), None)
        };
    if host.is_empty() {
        host = "127.0.0.1".to_string();
    }
    let port = match port_text {
        Some(text) => match text.parse::<u32>() {
            Ok(p) if (1..=65_535).contains(&p) => p as u16,
            _ => return Err(invalid()),
        },
        None => default_port,
    };
    Ok(DockerEndpoint::Tcp { host, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp(host: &str, port: u16) -> DockerEndpoint {
        DockerEndpoint::Tcp {
            host: host.into(),
            port,
        }
    }

    #[test]
    fn parses_every_scheme() {
        assert_eq!(
            DockerEndpoint::parse("unix:///var/run/docker.sock", false).unwrap(),
            DockerEndpoint::UnixSocket("/var/run/docker.sock".into())
        );
        assert_eq!(
            DockerEndpoint::parse("npipe:////./pipe/docker_engine", false).unwrap(),
            DockerEndpoint::NamedPipe(r"\\.\pipe\docker_engine".into())
        );
        assert_eq!(
            DockerEndpoint::parse("tcp://10.0.0.1:2375", false).unwrap(),
            tcp("10.0.0.1", 2375)
        );
        assert_eq!(
            DockerEndpoint::parse("tcp://[::1]:9000", false).unwrap(),
            tcp("::1", 9000)
        );
        assert_eq!(
            DockerEndpoint::parse("/tmp/d.sock", false).unwrap(),
            DockerEndpoint::UnixSocket("/tmp/d.sock".into())
        );
    }

    #[test]
    fn tcp_default_port_follows_tls() {
        assert_eq!(
            DockerEndpoint::parse("tcp://prod", false).unwrap(),
            tcp("prod", 2375)
        );
        assert_eq!(
            DockerEndpoint::parse("tcp://prod", true).unwrap(),
            tcp("prod", 2376)
        );
        assert_eq!(
            DockerEndpoint::parse("https://prod", false).unwrap(),
            tcp("prod", 2376)
        );
        assert_eq!(
            DockerEndpoint::parse("tcp://:2375", false).unwrap(),
            tcp("127.0.0.1", 2375)
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            DockerEndpoint::parse("ssh://me@box", false),
            Err(DockerError::UnsupportedEndpoint(_))
        ));
        assert!(DockerEndpoint::parse("tcp://h:99999", false).is_err());
        assert!(DockerEndpoint::parse("tcp://h:1/path", false).is_err());
        assert!(DockerEndpoint::parse("ftp://x", false).is_err());
        assert!(DockerEndpoint::parse("", false).is_err());
    }

    #[test]
    fn displays_like_docker_host() {
        assert_eq!(tcp("::1", 2375).to_string(), "tcp://[::1]:2375");
        assert_eq!(
            DockerEndpoint::NamedPipe(r"\\.\pipe\x".into()).to_string(),
            "npipe:////./pipe/x"
        );
        assert_eq!(tcp("::1", 1).host_header(), "[::1]:1");
        assert_eq!(
            DockerEndpoint::UnixSocket("/x".into()).host_header(),
            "docker"
        );
    }

    #[test]
    fn resolve_prefers_override_then_env_then_existing_socket() {
        let env = |k: &str| (k == "DOCKER_HOST").then(|| "tcp://e:1".to_string());
        let none = |_: &str| None;
        let exists = |p: &str| p.ends_with(".orbstack/run/docker.sock");
        assert_eq!(
            DockerEndpoint::resolve(Some("tcp://o:2"), &env, Platform::MacOS, None, &exists)
                .unwrap(),
            tcp("o", 2)
        );
        assert_eq!(
            DockerEndpoint::resolve(None, &env, Platform::MacOS, None, &exists).unwrap(),
            tcp("e", 1)
        );
        assert_eq!(
            DockerEndpoint::resolve(None, &none, Platform::MacOS, Some("/Users/me"), &exists)
                .unwrap(),
            DockerEndpoint::UnixSocket("/Users/me/.orbstack/run/docker.sock".into())
        );
        assert_eq!(
            DockerEndpoint::resolve(None, &none, Platform::Windows, None, &exists).unwrap(),
            DockerEndpoint::NamedPipe(DockerEndpoint::DEFAULT_WINDOWS_PIPE.into())
        );
    }

    #[test]
    fn locality() {
        assert!(tcp("localhost", 1).is_local());
        assert!(!tcp("prod", 1).is_local());
        assert!(DockerEndpoint::UnixSocket("/x".into()).is_local());
    }
}
