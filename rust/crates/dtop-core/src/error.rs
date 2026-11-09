//! Errors surfaced by the Docker client. Every message is written to be actionable for the person
//! at the terminal: what failed, and what they can do about it.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerError {
    InvalidEndpoint(String),
    UnsupportedEndpoint(String),
    ConnectionFailed {
        endpoint: String,
        reason: String,
    },
    SocketPathTooLong(String),
    Io(String),
    MalformedResponse(String),
    Http {
        status: u16,
        message: String,
    },
    Decoding(String),
    Cancelled,
    TimedOut(String),
    /// TLS setup or handshake failed. The message says what was wrong and how to fix it.
    Tls(String),
}

impl fmt::Display for DockerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DockerError::InvalidEndpoint(value) => write!(
                f,
                "Invalid Docker endpoint '{value}' — use unix:///path/to/docker.sock, \
                 npipe:////./pipe/docker_engine, tcp://host:port or https://host:2376 (via --host or DOCKER_HOST)"
            ),
            DockerError::UnsupportedEndpoint(value) => write!(
                f,
                "Unsupported Docker endpoint '{value}' — dtop connects over unix sockets, named pipes and \
                 tcp (with or without TLS); for ssh:// hosts, forward the socket first \
                 (ssh -NL /tmp/docker.sock:/var/run/docker.sock host) and use unix:///tmp/docker.sock"
            ),
            DockerError::ConnectionFailed { endpoint, reason } => write!(
                f,
                "Cannot connect to Docker at {endpoint} ({reason}) — is the Docker daemon running? \
                 Set DOCKER_HOST or pass --host to point dtop at another endpoint"
            ),
            DockerError::SocketPathTooLong(path) => write!(
                f,
                "Unix socket path is too long ({} bytes): {path} — use a shorter path or a symlink to the socket",
                path.len()
            ),
            DockerError::Io(reason) => write!(f, "I/O error while talking to Docker: {reason}"),
            DockerError::MalformedResponse(reason) => {
                write!(f, "Docker returned a response dtop could not parse: {reason}")
            }
            DockerError::Http { status, message } => {
                if message.contains("HTTP request to an HTTPS server") {
                    write!(
                        f,
                        "Docker API error {status}: {message} — this daemon expects TLS: add --tlsverify \
                         (with --tlscacert/--tlscert/--tlskey or DOCKER_CERT_PATH), or use https://"
                    )
                } else {
                    write!(f, "Docker API error {status}: {message}")
                }
            }
            DockerError::Decoding(reason) => write!(
                f,
                "Could not decode Docker API response: {reason} — your Docker version may be unsupported"
            ),
            DockerError::Cancelled => write!(f, "Request was cancelled"),
            DockerError::TimedOut(operation) => write!(
                f,
                "Timed out waiting for Docker during {operation} — the daemon may be overloaded"
            ),
            DockerError::Tls(reason) => write!(f, "TLS error: {reason}"),
        }
    }
}

impl std::error::Error for DockerError {}

impl DockerError {
    /// Whether retrying the same request later could reasonably succeed.
    pub fn is_transient(&self) -> bool {
        match self {
            DockerError::ConnectionFailed { .. }
            | DockerError::Io(_)
            | DockerError::TimedOut(_) => true,
            DockerError::Http { status, .. } => *status >= 500,
            _ => false,
        }
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, DockerError::Http { status: 404, .. })
    }
}

pub type Result<T, E = DockerError> = std::result::Result<T, E>;
