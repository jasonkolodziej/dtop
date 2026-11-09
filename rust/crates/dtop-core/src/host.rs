//! Docker hosts: endpoints with optional TLS, Docker CLI contexts, and the resolver that turns
//! flags, environment and contexts into concrete hosts the way the Docker CLI does.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::endpoint::DockerEndpoint;
use crate::error::{DockerError, Result};
use crate::json;
use crate::platform::{join_path, Platform};
use crate::sha256;

/// TLS settings for one TCP endpoint. Files are PEM, as Docker uses them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TlsConfiguration {
    /// Verify the daemon's certificate (`--tlsverify`). False encrypts without checking who the
    /// server is (`--tls`), as the Docker CLI does for `--tls` alone.
    pub verify_peer: bool,
    /// CA bundle to trust. None means the system's roots.
    pub ca_file: Option<String>,
    pub certificate_file: Option<String>,
    pub key_file: Option<String>,
    /// Name to verify the server certificate against. Defaults to the endpoint's host.
    pub server_name: Option<String>,
}

impl TlsConfiguration {
    pub fn has_client_certificate(&self) -> bool {
        self.certificate_file.is_some() && self.key_file.is_some()
    }

    /// Short description for headers and the host list.
    pub fn summary(&self) -> String {
        let mut parts = vec![if self.verify_peer {
            "TLS"
        } else {
            "TLS, unverified"
        }];
        if self.has_client_certificate() {
            parts.push("client cert");
        }
        parts.join(", ")
    }
}

/// One daemon dtop talks to: a name for the UI, the endpoint, and TLS when it applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerHost {
    pub name: String,
    pub endpoint: DockerEndpoint,
    pub tls: Option<TlsConfiguration>,
    /// Where it came from (`--host`, `DOCKER_HOST`, `context prod`, …), for messages.
    pub source: String,
    /// Explicit TLS settings supersede a context's stored settings for CLI operations too.
    pub context_tls_overridden: bool,
}

impl DockerHost {
    pub fn new(name: &str, endpoint: DockerEndpoint) -> Self {
        DockerHost {
            name: name.into(),
            endpoint,
            tls: None,
            source: String::new(),
            context_tls_overridden: false,
        }
    }

    /// Endpoint plus TLS mode, e.g. `tcp://prod:2376 (TLS, client cert)`.
    pub fn description(&self) -> String {
        match &self.tls {
            Some(tls) => format!("{} ({})", self.endpoint, tls.summary()),
            None => self.endpoint.to_string(),
        }
    }

    /// The Docker context this host came from, if any (`default` is not a stored context).
    pub fn context_name(&self) -> Option<&str> {
        let name = self.source.strip_prefix("context ")?;
        (name != "default").then_some(name)
    }

    /// Global `docker` CLI flags that reach this same daemon (used for `docker compose`):
    /// `--context` for contexts, else `--host` plus the TLS flags.
    pub fn docker_cli_arguments(&self) -> Vec<String> {
        if !self.context_tls_overridden {
            if let Some(context) = self.context_name() {
                return vec!["--context".into(), context.into()];
            }
        }
        let mut arguments = vec!["--host".to_string(), self.endpoint.to_string()];
        if let Some(tls) = &self.tls {
            arguments.push(
                if tls.verify_peer {
                    "--tlsverify"
                } else {
                    "--tls"
                }
                .into(),
            );
            for (flag, value) in [
                ("--tlscacert", &tls.ca_file),
                ("--tlscert", &tls.certificate_file),
                ("--tlskey", &tls.key_file),
            ] {
                if let Some(value) = value {
                    arguments.push(flag.into());
                    arguments.push(value.clone());
                }
            }
        }
        arguments
    }
}

/// TLS settings as the user gave them (flags, config). Unset fields fall back to the
/// environment and the certificate directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TlsRequest {
    /// `--tls`: encrypt (without verification unless `verify` is also set).
    pub enabled: Option<bool>,
    /// `--tlsverify`: encrypt and verify the server.
    pub verify: Option<bool>,
    pub ca_file: Option<String>,
    pub certificate_file: Option<String>,
    pub key_file: Option<String>,
    /// Directory holding `ca.pem`, `cert.pem` and `key.pem` (like `DOCKER_CERT_PATH`).
    pub certificate_directory: Option<String>,
}

impl TlsRequest {
    /// Fills unset fields from `defaults` (per-host settings override global ones).
    pub fn merged_over(&self, defaults: &TlsRequest) -> TlsRequest {
        TlsRequest {
            enabled: self.enabled.or(defaults.enabled),
            verify: self.verify.or(defaults.verify),
            ca_file: self.ca_file.clone().or_else(|| defaults.ca_file.clone()),
            certificate_file: self
                .certificate_file
                .clone()
                .or_else(|| defaults.certificate_file.clone()),
            key_file: self.key_file.clone().or_else(|| defaults.key_file.clone()),
            certificate_directory: self
                .certificate_directory
                .clone()
                .or_else(|| defaults.certificate_directory.clone()),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == TlsRequest::default()
    }
}

/// A host the user asked for: an address, a Docker context, or neither (the default host).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostRequest {
    pub name: Option<String>,
    pub address: Option<String>,
    pub context: Option<String>,
    pub tls: TlsRequest,
}

impl HostRequest {
    /// Parses `--host` values: `endpoint` or `name=endpoint`.
    pub fn parse(value: &str) -> HostRequest {
        if let Some((name, address)) = value.split_once('=') {
            let (name, address) = (name.trim(), address.trim());
            if !name.is_empty() && !name.contains('/') && !name.contains(':') && !address.is_empty()
            {
                return HostRequest {
                    name: Some(name.into()),
                    address: Some(address.into()),
                    ..Default::default()
                };
            }
        }
        HostRequest {
            address: Some(value.into()),
            ..Default::default()
        }
    }
}

/// Reads a file's text (None when missing or unreadable).
pub type ReadFile = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
/// Lists a directory's entry names (empty when missing).
pub type ListDirectory = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

/// What the resolver may look at, injected so resolution is testable.
#[derive(Clone)]
pub struct HostEnvironment {
    pub variables: HashMap<String, String>,
    pub platform: Platform,
    pub home_directory: Option<String>,
    pub file_exists: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub read_file: ReadFile,
    pub list_directory: ListDirectory,
}

impl HostEnvironment {
    /// The real process environment and file system.
    pub fn system() -> HostEnvironment {
        HostEnvironment {
            variables: std::env::vars().collect(),
            platform: Platform::current(),
            home_directory: home_directory(),
            file_exists: Arc::new(|p| std::path::Path::new(p).exists()),
            read_file: Arc::new(|p| std::fs::read_to_string(p).ok()),
            list_directory: Arc::new(|p| {
                std::fs::read_dir(p)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok()?.file_name().into_string().ok())
                            .collect()
                    })
                    .unwrap_or_default()
            }),
        }
    }

    /// A non-empty variable.
    pub fn variable(&self, name: &str) -> Option<String> {
        self.variables
            .get(name)
            .filter(|v| !v.trim().is_empty())
            .cloned()
    }

    pub fn join(&self, parts: &[&str]) -> String {
        join_path(parts, self.platform)
    }
}

/// `HOME`, or `USERPROFILE` on Windows.
pub fn home_directory() -> Option<String> {
    let names: &[&str] = if cfg!(windows) {
        &["USERPROFILE", "HOME"]
    } else {
        &["HOME"]
    };
    names
        .iter()
        .find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
}

/// A Docker CLI context, as stored under `~/.docker/contexts`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DockerContext {
    pub name: String,
    pub description: Option<String>,
    /// The context's Docker endpoint (`Endpoints.docker.Host`).
    pub host: Option<String>,
    pub skip_tls_verify: bool,
    pub ca_file: Option<String>,
    pub certificate_file: Option<String>,
    pub key_file: Option<String>,
}

impl DockerContext {
    /// Whether `docker context create --docker "…,ca=…,cert=…,key=…"` stored TLS files.
    pub fn has_tls_material(&self) -> bool {
        self.ca_file.is_some() || self.certificate_file.is_some() || self.key_file.is_some()
    }
}

/// Reads contexts the way the Docker CLI stores them:
/// - `$DOCKER_CONFIG` (default `~/.docker`) `/config.json` names the `currentContext`;
///   `DOCKER_CONTEXT` overrides it.
/// - `contexts/meta/<sha256(name)>/meta.json` holds the name and endpoint.
/// - `contexts/tls/<sha256(name)>/docker/{ca,cert,key}.pem` holds TLS material.
pub struct DockerContextStore<'a> {
    pub environment: &'a HostEnvironment,
}

impl DockerContextStore<'_> {
    pub fn config_directory(&self) -> Option<String> {
        self.environment.variable("DOCKER_CONFIG").or_else(|| {
            self.environment
                .home_directory
                .as_deref()
                .map(|h| self.environment.join(&[h, ".docker"]))
        })
    }

    pub fn meta_directory(&self) -> Option<String> {
        self.config_directory()
            .map(|c| self.environment.join(&[&c, "contexts", "meta"]))
    }

    /// `DOCKER_CONTEXT`, else `currentContext` from config.json, else None (the default).
    pub fn active_context_name(&self) -> Option<String> {
        if let Some(name) = self.environment.variable("DOCKER_CONTEXT") {
            return Some(name.trim().to_string());
        }
        let directory = self.config_directory()?;
        let text =
            (self.environment.read_file)(&self.environment.join(&[&directory, "config.json"]))?;
        let config = json::parse_str(&text).ok()?;
        config
            .get("currentContext")
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    pub fn context(&self, name: &str) -> Option<DockerContext> {
        let meta = self.meta_directory()?;
        self.load(&self.environment.join(&[&meta, &sha256::hex_digest(name)]))
    }

    /// Every stored context, sorted by name (the built-in `default` is not stored).
    pub fn list(&self) -> Vec<DockerContext> {
        let Some(meta) = self.meta_directory() else {
            return Vec::new();
        };
        let mut contexts: Vec<DockerContext> = (self.environment.list_directory)(&meta)
            .iter()
            .filter_map(|entry| self.load(&self.environment.join(&[&meta, entry])))
            .collect();
        contexts.sort_by(|a, b| a.name.cmp(&b.name));
        contexts
    }

    fn load(&self, directory: &str) -> Option<DockerContext> {
        let text = (self.environment.read_file)(&self.environment.join(&[directory, "meta.json"]))?;
        let meta = json::parse_str(&text).ok()?;
        let name = meta.get("Name").as_str()?.to_string();
        let endpoint = meta.get("Endpoints").get("docker");
        let mut context = DockerContext {
            description: meta.get("Metadata").opt_str("Description"),
            host: endpoint.opt_str("Host"),
            skip_tls_verify: endpoint.bool_or("SkipTLSVerify", false),
            name,
            ..Default::default()
        };
        if let Some(config) = self.config_directory() {
            let digest = sha256::hex_digest(&context.name);
            let tls_directory = self
                .environment
                .join(&[&config, "contexts", "tls", &digest, "docker"]);
            let file = |n: &str| {
                let path = self.environment.join(&[&tls_directory, n]);
                (self.environment.file_exists)(&path).then_some(path)
            };
            context.ca_file = file("ca.pem");
            context.certificate_file = file("cert.pem");
            context.key_file = file("key.pem");
        }
        Some(context)
    }
}

/// The resolved hosts plus hosts that were skipped (e.g. ssh:// contexts), with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolution {
    pub hosts: Vec<DockerHost>,
    pub warnings: Vec<String>,
}

/// Turns host requests into hosts, following the Docker CLI:
/// - Address: the request's, else `DOCKER_HOST`, else the active context, else the first
///   well-known socket that exists.
/// - TLS: on with `--tls`/`--tlsverify`, `DOCKER_TLS_VERIFY` (verify), `DOCKER_TLS`, or an
///   `https://` address. Certificates from explicit paths, else `DOCKER_CERT_PATH`, else
///   `~/.docker` (`ca.pem`, `cert.pem`, `key.pem`, each when present).
/// - Contexts carry their own endpoint and TLS material.
pub fn resolve_hosts(
    requests: &[HostRequest],
    global_tls: &TlsRequest,
    all_contexts: bool,
    environment: &HostEnvironment,
) -> Result<Resolution> {
    let mut hosts = Vec::new();
    let mut warnings = Vec::new();
    let default_request = [HostRequest::default()];
    let effective: &[HostRequest] = if requests.is_empty() && !all_contexts {
        &default_request
    } else {
        requests
    };
    for request in effective {
        hosts.push(resolve_host(request, global_tls, environment)?);
    }
    if all_contexts {
        let store = DockerContextStore { environment };
        if !hosts.iter().any(|h| h.source == "context default")
            && !requests
                .iter()
                .any(|r| r.context.as_deref() == Some("default"))
        {
            let request = HostRequest {
                name: Some("default".into()),
                ..Default::default()
            };
            let mut default_host = resolve_address(&request, global_tls, environment, true)?;
            default_host.source = "context default".into();
            hosts.insert(0, default_host);
        }
        for context in store.list() {
            let source = format!("context {}", context.name);
            if hosts.iter().any(|h| h.source == source) {
                continue;
            }
            match resolve_context(
                &context,
                None,
                &TlsRequest::default(),
                global_tls,
                environment,
            ) {
                Ok(host) => hosts.push(host),
                Err(error) => warnings.push(format!("context {}: {error}", context.name)),
            }
        }
    }
    Ok(Resolution {
        hosts: uniquely_named(hosts),
        warnings,
    })
}

/// Resolves one request.
pub fn resolve_host(
    request: &HostRequest,
    global_tls: &TlsRequest,
    environment: &HostEnvironment,
) -> Result<DockerHost> {
    let store = DockerContextStore { environment };
    if let Some(context_name) = &request.context {
        if context_name == "default" {
            let named = HostRequest {
                name: Some(request.name.clone().unwrap_or_else(|| "default".into())),
                tls: request.tls.clone(),
                ..Default::default()
            };
            let mut host = resolve_address(&named, global_tls, environment, true)?;
            host.source = "context default".into();
            return Ok(host);
        }
        let context = store
            .context(context_name)
            .ok_or_else(|| missing_context(context_name, &store))?;
        return resolve_context(
            &context,
            request.name.as_deref(),
            &request.tls,
            global_tls,
            environment,
        );
    }
    // A bare default request follows DOCKER_HOST, then the active context.
    if request.address.is_none() && environment.variable("DOCKER_HOST").is_none() {
        if let Some(active) = store.active_context_name().filter(|n| n != "default") {
            let context = store
                .context(&active)
                .ok_or_else(|| missing_context(&active, &store))?;
            return resolve_context(
                &context,
                request.name.as_deref(),
                &request.tls,
                global_tls,
                environment,
            );
        }
    }
    resolve_address(request, global_tls, environment, true)
}

fn missing_context(name: &str, store: &DockerContextStore<'_>) -> DockerError {
    let place = store
        .meta_directory()
        .unwrap_or_else(|| "~/.docker/contexts".into());
    DockerError::InvalidContext(format!(
        "context '{name}' was not found in {place} — run docker context ls or select an existing context"
    ))
}

fn resolve_context(
    context: &DockerContext,
    name: Option<&str>,
    request_tls: &TlsRequest,
    global_tls: &TlsRequest,
    environment: &HostEnvironment,
) -> Result<DockerHost> {
    let mut tls_request = request_tls.merged_over(global_tls);
    if tls_request.is_empty() {
        return host_for_context(context, name);
    }
    let address = context.host.as_ref().ok_or_else(|| {
        DockerError::InvalidContext(format!(
            "context '{}' has no Docker endpoint — recreate it with docker context create",
            context.name
        ))
    })?;
    let context_tls = context.has_tls_material() || DockerEndpoint::scheme_implies_tls(address);
    if !DockerEndpoint::parse(address, context_tls)?.is_tcp() {
        return host_for_context(context, name);
    }
    if tls_request.certificate_directory.is_none() {
        tls_request.ca_file = tls_request.ca_file.or_else(|| context.ca_file.clone());
        tls_request.certificate_file = tls_request
            .certificate_file
            .or_else(|| context.certificate_file.clone());
        tls_request.key_file = tls_request.key_file.or_else(|| context.key_file.clone());
    }
    if context_tls {
        tls_request.verify = tls_request.verify.or(Some(!context.skip_tls_verify));
        tls_request.enabled = tls_request.enabled.or(Some(true));
    }
    let request = HostRequest {
        name: Some(name.unwrap_or(&context.name).to_string()),
        address: Some(address.clone()),
        tls: tls_request,
        ..Default::default()
    };
    let mut host = resolve_address(&request, &TlsRequest::default(), environment, false)?;
    host.source = format!("context {}", context.name);
    host.context_tls_overridden = true;
    Ok(host)
}

fn resolve_address(
    request: &HostRequest,
    global_tls: &TlsRequest,
    environment: &HostEnvironment,
    use_environment_host: bool,
) -> Result<DockerHost> {
    let tls_request = request.tls.merged_over(global_tls);
    let (address, source) = if let Some(explicit) = &request.address {
        (Some(explicit.clone()), "--host")
    } else if let Some(from_env) = environment
        .variable("DOCKER_HOST")
        .filter(|_| use_environment_host)
    {
        (Some(from_env), "DOCKER_HOST")
    } else {
        (None, "default socket")
    };
    let verify = tls_request
        .verify
        .unwrap_or_else(|| environment.variable("DOCKER_TLS_VERIFY").is_some());
    let enabled = tls_request
        .enabled
        .unwrap_or_else(|| environment.variable("DOCKER_TLS").is_some());
    let wants_tls = verify
        || enabled
        || address
            .as_deref()
            .is_some_and(DockerEndpoint::scheme_implies_tls);
    let endpoint = match &address {
        Some(address) => DockerEndpoint::parse(address, wants_tls)?,
        None => DockerEndpoint::resolve(
            None,
            &|name| {
                (name != "DOCKER_HOST")
                    .then(|| environment.variable(name))
                    .flatten()
            },
            environment.platform,
            environment.home_directory.as_deref(),
            &|p| (environment.file_exists)(p),
        )?,
    };
    let tls = if wants_tls && endpoint.is_tcp() {
        Some(tls_files(&tls_request, verify, environment)?)
    } else {
        None
    };
    let name = request
        .name
        .clone()
        .unwrap_or_else(|| default_name(&endpoint));
    Ok(DockerHost {
        name,
        endpoint,
        tls,
        source: source.into(),
        context_tls_overridden: false,
    })
}

/// Picks CA, certificate and key files. Explicit paths must exist; defaults are used only when
/// present. A certificate needs its key and vice versa.
fn tls_files(
    request: &TlsRequest,
    verify: bool,
    environment: &HostEnvironment,
) -> Result<TlsConfiguration> {
    let directory = request
        .certificate_directory
        .clone()
        .or_else(|| environment.variable("DOCKER_CERT_PATH"))
        .or_else(|| {
            environment
                .home_directory
                .as_deref()
                .map(|h| environment.join(&[h, ".docker"]))
        });
    let pick = |explicit: &Option<String>, file_name: &str, flag: &str| -> Result<Option<String>> {
        if let Some(explicit) = explicit {
            if !(environment.file_exists)(explicit) {
                return Err(DockerError::Tls(format!(
                    "{flag} file {explicit} does not exist"
                )));
            }
            return Ok(Some(explicit.clone()));
        }
        let Some(directory) = &directory else {
            return Ok(None);
        };
        let candidate = environment.join(&[directory, file_name]);
        Ok((environment.file_exists)(&candidate).then_some(candidate))
    };
    let ca = pick(&request.ca_file, "ca.pem", "--tlscacert")?;
    let certificate = pick(&request.certificate_file, "cert.pem", "--tlscert")?;
    let key = pick(&request.key_file, "key.pem", "--tlskey")?;
    if certificate.is_none() != key.is_none() {
        let missing = if certificate.is_none() {
            "certificate (cert.pem / --tlscert)"
        } else {
            "key (key.pem / --tlskey)"
        };
        return Err(DockerError::Tls(format!(
            "a client certificate needs both a certificate and a key; the {missing} is missing"
        )));
    }
    Ok(TlsConfiguration {
        verify_peer: verify,
        ca_file: ca,
        certificate_file: certificate,
        key_file: key,
        server_name: None,
    })
}

fn host_for_context(context: &DockerContext, name: Option<&str>) -> Result<DockerHost> {
    let Some(address) = &context.host else {
        return Err(DockerError::InvalidContext(format!(
            "context '{}' has no Docker endpoint — recreate it with docker context create",
            context.name
        )));
    };
    let endpoint = DockerEndpoint::parse(address, context.has_tls_material())?;
    if endpoint.is_tcp() && context.certificate_file.is_some() != context.key_file.is_some() {
        let missing = if context.certificate_file.is_none() {
            "certificate (cert.pem)"
        } else {
            "key (key.pem)"
        };
        return Err(DockerError::Tls(format!(
            "context '{}' has incomplete client TLS material: missing {missing} — recreate the Docker context",
            context.name
        )));
    }
    let tls = (endpoint.is_tcp()
        && (context.has_tls_material() || DockerEndpoint::scheme_implies_tls(address)))
    .then(|| TlsConfiguration {
        verify_peer: !context.skip_tls_verify,
        ca_file: context.ca_file.clone(),
        certificate_file: context.certificate_file.clone(),
        key_file: context.key_file.clone(),
        server_name: None,
    });
    Ok(DockerHost {
        name: name.map_or_else(|| context.name.clone(), str::to_string),
        endpoint,
        tls,
        source: format!("context {}", context.name),
        context_tls_overridden: false,
    })
}

/// `local` for sockets and pipes, the host name for TCP.
pub fn default_name(endpoint: &DockerEndpoint) -> String {
    match endpoint {
        DockerEndpoint::UnixSocket(_) | DockerEndpoint::NamedPipe(_) => "local".into(),
        DockerEndpoint::Tcp { host, .. } => host.clone(),
    }
}

/// Appends `-2`, `-3`, … to repeated names so every host has a distinct name.
fn uniquely_named(hosts: Vec<DockerHost>) -> Vec<DockerHost> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut used = HashSet::new();
    hosts
        .into_iter()
        .map(|mut host| {
            let base = host.name.clone();
            let count = counts.entry(base.clone()).or_insert(1);
            while !used.insert(host.name.clone()) {
                *count += 1;
                host.name = format!("{base}-{count}");
            }
            host
        })
        .collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// An in-memory file system for resolver tests.
    pub fn environment(variables: &[(&str, &str)], files: &[(&str, &str)]) -> HostEnvironment {
        let files: Arc<BTreeMap<String, String>> = Arc::new(
            files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        let (f1, f2, f3) = (Arc::clone(&files), Arc::clone(&files), files);
        HostEnvironment {
            variables: variables
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            platform: Platform::Linux,
            home_directory: Some("/home/me".into()),
            file_exists: Arc::new(move |p| {
                f1.contains_key(p) || f1.keys().any(|k| k.starts_with(&format!("{p}/")))
            }),
            read_file: Arc::new(move |p| f2.get(p).cloned()),
            list_directory: Arc::new(move |p| {
                let prefix = format!("{p}/");
                let mut names: Vec<String> = f3
                    .keys()
                    .filter_map(|k| {
                        k.strip_prefix(&prefix)
                            .and_then(|rest| rest.split('/').next())
                            .map(str::to_string)
                    })
                    .collect();
                names.dedup();
                names
            }),
        }
    }

    fn context_files(name: &str, host: &str, tls: bool) -> Vec<(String, String)> {
        let digest = sha256::hex_digest(name);
        let mut files = vec![(
            format!("/home/me/.docker/contexts/meta/{digest}/meta.json"),
            format!(
                r#"{{"Name":"{name}","Endpoints":{{"docker":{{"Host":"{host}","SkipTLSVerify":false}}}}}}"#
            ),
        )];
        if tls {
            for f in ["ca.pem", "cert.pem", "key.pem"] {
                files.push((
                    format!("/home/me/.docker/contexts/tls/{digest}/docker/{f}"),
                    "x".into(),
                ));
            }
        }
        files
    }

    fn env_with(variables: &[(&str, &str)], files: Vec<(String, String)>) -> HostEnvironment {
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        environment(variables, &refs)
    }

    #[test]
    fn default_host_uses_docker_host_and_tls_env() {
        let env = env_with(
            &[
                ("DOCKER_HOST", "tcp://prod"),
                ("DOCKER_TLS_VERIFY", "1"),
                ("DOCKER_CERT_PATH", "/certs"),
            ],
            vec![
                ("/certs/ca.pem".into(), "x".into()),
                ("/certs/cert.pem".into(), "x".into()),
                ("/certs/key.pem".into(), "x".into()),
            ],
        );
        let r = resolve_hosts(&[], &TlsRequest::default(), false, &env).unwrap();
        let host = &r.hosts[0];
        assert_eq!(
            host.endpoint,
            DockerEndpoint::Tcp {
                host: "prod".into(),
                port: 2376
            }
        );
        assert_eq!(host.name, "prod");
        let tls = host.tls.as_ref().unwrap();
        assert!(tls.verify_peer);
        assert_eq!(tls.ca_file.as_deref(), Some("/certs/ca.pem"));
        assert_eq!(host.description(), "tcp://prod:2376 (TLS, client cert)");
    }

    #[test]
    fn active_context_is_used_when_no_docker_host() {
        let mut files = context_files("prod", "tcp://prod.example:2376", true);
        files.push((
            "/home/me/.docker/config.json".into(),
            r#"{"currentContext":"prod"}"#.into(),
        ));
        let env = env_with(&[], files);
        let host = resolve_host(&HostRequest::default(), &TlsRequest::default(), &env).unwrap();
        assert_eq!(host.name, "prod");
        assert_eq!(host.context_name(), Some("prod"));
        assert_eq!(host.docker_cli_arguments(), vec!["--context", "prod"]);
        assert!(host.tls.as_ref().unwrap().verify_peer);
    }

    #[test]
    fn missing_active_context_does_not_fall_back_to_the_local_daemon() {
        let env = env_with(
            &[("DOCKER_CONTEXT", "missing")],
            vec![("/var/run/docker.sock".into(), String::new())],
        );
        let error = resolve_host(&HostRequest::default(), &TlsRequest::default(), &env)
            .expect_err("a missing active context must not select a different daemon");
        assert!(error.to_string().contains("context 'missing'"));
    }

    #[test]
    fn default_host_discovers_the_rootless_runtime_socket() {
        let env = env_with(
            &[("XDG_RUNTIME_DIR", "/run/user/1000")],
            vec![("/run/user/1000/docker.sock".into(), String::new())],
        );
        let host = resolve_host(&HostRequest::default(), &TlsRequest::default(), &env).unwrap();
        assert_eq!(
            host.endpoint,
            DockerEndpoint::UnixSocket("/run/user/1000/docker.sock".into())
        );
    }

    #[test]
    fn explicit_context_tls_overrides_keep_the_context_client_certificate() {
        let mut files = context_files("prod", "tcp://prod.example:2376", true);
        files.push(("/override/ca.pem".into(), "x".into()));
        let env = env_with(&[], files);
        let request = HostRequest {
            context: Some("prod".into()),
            ..Default::default()
        };
        let override_tls = TlsRequest {
            verify: Some(false),
            ca_file: Some("/override/ca.pem".into()),
            ..Default::default()
        };
        let host = resolve_host(&request, &override_tls, &env).unwrap();
        let tls = host.tls.as_ref().unwrap();
        let directory = format!(
            "/home/me/.docker/contexts/tls/{}/docker",
            sha256::hex_digest("prod")
        );
        assert!(!tls.verify_peer);
        assert_eq!(tls.ca_file.as_deref(), Some("/override/ca.pem"));
        assert_eq!(
            tls.certificate_file.as_deref(),
            Some(format!("{directory}/cert.pem").as_str())
        );
        assert_eq!(
            tls.key_file.as_deref(),
            Some(format!("{directory}/key.pem").as_str())
        );
        assert_eq!(host.context_name(), Some("prod"));
        assert_eq!(
            host.docker_cli_arguments(),
            vec![
                "--host".to_string(),
                "tcp://prod.example:2376".into(),
                "--tls".into(),
                "--tlscacert".into(),
                "/override/ca.pem".into(),
                "--tlscert".into(),
                format!("{directory}/cert.pem"),
                "--tlskey".into(),
                format!("{directory}/key.pem"),
            ]
        );
    }

    #[test]
    fn all_contexts_apply_global_tls_flags_to_tcp_hosts() {
        let mut files = context_files("prod", "tcp://prod.example:2376", false);
        files.extend([
            ("/var/run/docker.sock".into(), String::new()),
            ("/override/ca.pem".into(), "x".into()),
        ]);
        let env = env_with(&[], files);
        let global_tls = TlsRequest {
            verify: Some(true),
            ca_file: Some("/override/ca.pem".into()),
            ..Default::default()
        };
        let result = resolve_hosts(&[], &global_tls, true, &env).unwrap();
        let host = result
            .hosts
            .iter()
            .find(|host| host.name == "prod")
            .unwrap();
        let tls = host.tls.as_ref().expect("global --tlsverify must apply");
        assert!(tls.verify_peer);
        assert_eq!(tls.ca_file.as_deref(), Some("/override/ca.pem"));
        assert_eq!(host.docker_cli_arguments()[0], "--host");
    }

    #[test]
    fn context_with_only_half_a_client_certificate_pair_is_rejected() {
        let mut files = context_files("prod", "tcp://prod.example:2376", false);
        let directory = format!(
            "/home/me/.docker/contexts/tls/{}/docker",
            sha256::hex_digest("prod")
        );
        files.push((format!("{directory}/cert.pem"), "x".into()));
        let env = env_with(&[], files);
        let request = HostRequest {
            context: Some("prod".into()),
            ..Default::default()
        };
        let error = resolve_host(&request, &TlsRequest::default(), &env).unwrap_err();
        assert!(error.to_string().contains("key"));
    }

    #[test]
    fn all_contexts_skips_ssh_with_warning() {
        let mut files = context_files("remote", "ssh://me@box", false);
        files.extend(context_files("prod", "tcp://p:2375", false));
        let env = env_with(&[], files);
        let r = resolve_hosts(&[], &TlsRequest::default(), true, &env).unwrap();
        let names: Vec<_> = r.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["default", "prod"]);
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].starts_with("context remote:"));
    }

    #[test]
    fn explicit_missing_cert_fails_and_half_pairs_fail() {
        let env = env_with(&[], vec![("/c/cert.pem".into(), "x".into())]);
        let mut request = HostRequest::parse("tcp://h");
        request.tls.verify = Some(true);
        request.tls.ca_file = Some("/nope/ca.pem".into());
        assert!(matches!(
            resolve_host(&request, &TlsRequest::default(), &env),
            Err(DockerError::Tls(_))
        ));
        request.tls.ca_file = None;
        request.tls.certificate_file = Some("/c/cert.pem".into());
        let err = resolve_host(&request, &TlsRequest::default(), &env).unwrap_err();
        assert!(err
            .to_string()
            .contains("key (key.pem / --tlskey) is missing"));
    }

    #[test]
    fn names_hosts_uniquely_and_parses_named_hosts() {
        let env = env_with(&[], vec![]);
        let requests = vec![
            HostRequest::parse("tcp://a:1"),
            HostRequest::parse("tcp://a:2"),
            HostRequest::parse("x=tcp://b:3"),
        ];
        let r = resolve_hosts(&requests, &TlsRequest::default(), false, &env).unwrap();
        let names: Vec<_> = r.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["a", "a-2", "x"]);
        assert_eq!(
            HostRequest::parse("unix:///x=y").address.as_deref(),
            Some("unix:///x=y")
        );
    }

    #[test]
    fn generated_host_suffixes_do_not_collide_with_existing_names() {
        let env = env_with(&[], vec![]);
        let requests =
            ["a=tcp://one:1", "a-2=tcp://two:1", "a=tcp://three:1"].map(HostRequest::parse);
        let result = resolve_hosts(&requests, &TlsRequest::default(), false, &env).unwrap();
        let names: Vec<_> = result.hosts.iter().map(|host| host.name.as_str()).collect();
        assert_eq!(names, vec!["a", "a-2", "a-3"]);
    }

    #[test]
    fn https_implies_tls_and_cli_arguments_carry_flags() {
        let env = env_with(&[], vec![]);
        let host = resolve_host(
            &HostRequest::parse("https://h"),
            &TlsRequest::default(),
            &env,
        )
        .unwrap();
        assert_eq!(
            host.endpoint,
            DockerEndpoint::Tcp {
                host: "h".into(),
                port: 2376
            }
        );
        assert_eq!(
            host.docker_cli_arguments(),
            vec!["--host", "tcp://h:2376", "--tls"]
        );
    }
}
