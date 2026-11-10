//! Docker transports, TLS, hosts and contexts, the HTTP client, the Engine API client and its
//! models, metrics math, GPU collectors and compose support for dtop. Only `std` and the
//! platform C libraries (ADR 0001, ADR 0004).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod api;
pub mod client;
pub mod compose;
pub mod endpoint;
pub mod error;
pub mod format;
pub mod gpu;
pub mod gpu_exporter;
pub mod host;
pub mod http;
pub mod json;
pub mod logstream;
pub mod models;
pub mod monitoring;
pub mod platform;
pub mod sha256;
pub mod timestamp;
pub mod tls;
pub mod transport;

pub use api::{
    ApiVersion, ContainerAction, DockerApi, StreamCancellation, StreamItem, Subscription,
};
pub use client::DockerClient;
pub use endpoint::DockerEndpoint;
pub use error::{DockerError, Result};
pub use host::{DockerHost, HostEnvironment, HostRequest, TlsConfiguration, TlsRequest};
pub use platform::Platform;
