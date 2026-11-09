//! Settings from `dtop.conf` and the command line.

use std::collections::HashMap;
use std::fmt;

use dtop_core::host::{HostRequest, TlsRequest};
use dtop_core::platform::{join_path, Platform};
use dtop_ui::ColorMode;

use crate::state::SortKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMode {
    Interactive,
    Once,
    Help,
    Version,
    ListThemes,
    PrintTheme(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationError(pub String);

impl fmt::Display for ConfigurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigurationError {}

type Result<T> = std::result::Result<T, ConfigurationError>;

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(ConfigurationError(message.into()))
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfiguration {
    /// Hosts to monitor (`--host`, `--context`, `host =`, `[host NAME]`). Empty means the
    /// default host: DOCKER_HOST, the active Docker context, or the local socket.
    pub hosts: Vec<HostRequest>,
    /// TLS flags for every host (`--tls`, `--tlsverify`, `--tlscacert`, …).
    pub global_tls: TlsRequest,
    /// Monitor every Docker context (`--all-contexts`).
    pub all_contexts: bool,
    /// Set once the command line names a host, so it replaces (not extends) the file's hosts.
    hosts_from_command_line: bool,
    pub refresh_interval: f64,
    pub theme_name: String,
    pub color_mode: Option<ColorMode>,
    pub mouse: bool,
    /// Collect GPU metrics.
    pub gpu: bool,
    pub show_stopped: bool,
    pub sort_key: SortKey,
    /// None means "the natural direction for the sort key".
    pub sort_descending: Option<bool>,
    pub history_size: usize,
    pub config_file_path: Option<String>,
    pub mode: RunMode,
}

impl Default for AppConfiguration {
    fn default() -> Self {
        AppConfiguration {
            hosts: Vec::new(),
            global_tls: TlsRequest::default(),
            all_contexts: false,
            hosts_from_command_line: false,
            refresh_interval: 2.0,
            theme_name: "harbor".into(),
            color_mode: None,
            mouse: true,
            gpu: true,
            show_stopped: true,
            sort_key: SortKey::Cpu,
            sort_descending: None,
            history_size: 240,
            config_file_path: None,
            mode: RunMode::Interactive,
        }
    }
}

pub const INTERVAL_RANGE: (f64, f64) = (0.25, 60.0);
pub const HISTORY_RANGE: (usize, usize) = (30, 3600);

pub const USAGE: &str = "dtop — Docker-first terminal observability and control

USAGE:
  dtop [options]

OPTIONS:
  -H, --host [name=]<endpoint>
                            Docker endpoint (unix:///var/run/docker.sock, npipe:////./pipe/docker_engine,
                            tcp://host:2375, https://host:2376). Repeat to monitor several hosts.
                            Defaults to DOCKER_HOST, then the active Docker context, then local sockets.
      --context <name>      Monitor a Docker context (repeatable; see docker context ls)
      --all-contexts        Monitor every Docker context
      --tls                 Use TLS (without verifying the daemon unless --tlsverify)
      --tlsverify           Use TLS and verify the daemon (also DOCKER_TLS_VERIFY=1)
      --tlscacert <file>    CA that issued the daemon certificate (default ~/.docker/ca.pem)
      --tlscert <file>      Client certificate (default ~/.docker/cert.pem)
      --tlskey <file>       Client key (default ~/.docker/key.pem; DOCKER_CERT_PATH moves all three)
  -i, --interval <seconds>  Refresh interval, 0.25-60 (default 2; accepts 500ms)
  -t, --theme <name|file>   Built-in theme name or path to a .theme file (default harbor)
      --color <mode>        auto, truecolor, 256, 16 or none (default auto; honours NO_COLOR)
      --running             Only list running containers
      --sort <key>          Initial sort: name, cpu, memory, gpu, status, created, image
      --no-mouse            Disable mouse reporting
      --no-gpu              Don't collect GPU metrics (NVIDIA, AMD, Intel, Apple)
      --history <samples>   Samples kept per graph (30-3600, default 240)
  -c, --config <file>       Read settings from this file instead of the default location
      --once                Print one plain-text snapshot and exit (scriptable)
      --list-themes         List built-in themes
      --print-theme <name>  Print a built-in theme in theme-file format (a starting point for your own)
  -h, --help                Show this help
  -v, --version             Show version

KEYS (press ? inside dtop for the full list):
  ↑/↓ j/k move · ←/→ Tab switch views · / filter · s sort · Enter details · [ ] H hosts · T themes · q quit";

pub fn parse_bool(value: &str, key: &str) -> Result<bool> {
    match value.to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => err(format!("{key} must be true or false (got '{value}')")),
    }
}

pub fn parse_interval(value: &str) -> Result<f64> {
    let lowered = value.to_lowercase();
    let (text, multiplier) = if let Some(t) = lowered.strip_suffix("ms") {
        (t, 0.001)
    } else if let Some(t) = lowered.strip_suffix('s') {
        (t, 1.0)
    } else {
        (lowered.as_str(), 1.0)
    };
    let number: f64 = match text.parse() {
        Ok(n) if f64::is_finite(n) => n,
        _ => {
            return err(format!(
                "invalid interval '{value}' — use seconds such as 2, 0.5 or 500ms"
            ))
        }
    };
    let seconds = number * multiplier;
    if !(INTERVAL_RANGE.0..=INTERVAL_RANGE.1).contains(&seconds) {
        return err(format!(
            "interval must be between {}s and {}s (got {value})",
            INTERVAL_RANGE.0, INTERVAL_RANGE.1 as i64
        ));
    }
    Ok(seconds)
}

/// TLS keys shared by the global settings and `[host]` sections. Ok(false) for other keys.
fn apply_tls_key(key: &str, value: &str, tls: &mut TlsRequest) -> Result<bool> {
    let optional = || (!value.is_empty()).then(|| value.to_string());
    match key {
        "tls" => tls.enabled = Some(parse_bool(value, key)?),
        "tlsverify" | "tls_verify" => tls.verify = Some(parse_bool(value, key)?),
        "tlscacert" | "tls_ca_cert" | "tls_ca" => tls.ca_file = optional(),
        "tlscert" | "tls_cert" => tls.certificate_file = optional(),
        "tlskey" | "tls_key" => tls.key_file = optional(),
        "cert_path" | "tls_cert_path" | "docker_cert_path" => {
            tls.certificate_directory = optional()
        }
        _ => return Ok(false),
    }
    Ok(true)
}

impl AppConfiguration {
    /// The first host's address, for single-host callers.
    pub fn host(&self) -> Option<&str> {
        self.hosts.first().and_then(|h| h.address.as_deref())
    }

    /// Applies `dtop.conf`. Unknown keys are errors so typos surface. `[host NAME]` starts a
    /// section describing one more host.
    pub fn apply_config_file(&mut self, text: &str) -> Result<()> {
        let mut section: Option<usize> = None;
        for (offset, raw) in text.split('\n').enumerate() {
            let line_number = offset + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') {
                if !line.ends_with(']') {
                    return err(format!(
                        "dtop.conf line {line_number}: expected '[host NAME]', found '{line}'"
                    ));
                }
                let words: Vec<&str> = line[1..line.len() - 1].split_whitespace().collect();
                if words.len() != 2 || words[0].to_lowercase() != "host" {
                    return err(format!("dtop.conf line {line_number}: sections must be '[host NAME]', found '{line}'"));
                }
                self.hosts.push(HostRequest {
                    name: Some(words[1].to_string()),
                    ..Default::default()
                });
                section = Some(self.hosts.len() - 1);
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return err(format!(
                    "dtop.conf line {line_number}: expected 'key = value', found '{line}'"
                ));
            };
            let key = key.trim().to_lowercase().replace('-', "_");
            let mut value = value.trim();
            if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                value = &value[1..value.len() - 1];
            }
            let result = match section {
                Some(index) => apply_host_key(&key, value, &mut self.hosts[index]),
                None => self.apply_key(&key, value),
            };
            if let Err(error) = result {
                return err(format!("dtop.conf line {line_number}: {}", error.0));
            }
        }
        for (index, host) in self.hosts.iter().enumerate() {
            let Some(name) = &host.name else { continue };
            if host.address.is_none() && host.context.is_none() {
                return err(format!(
                    "dtop.conf: [host {name}] needs 'endpoint = …' or 'context = …' (host {})",
                    index + 1
                ));
            }
            if host.address.is_some() && host.context.is_some() {
                return err(format!(
                    "dtop.conf: [host {name}] sets both 'endpoint' and 'context' — a context already names its endpoint; keep one of them"
                ));
            }
        }
        Ok(())
    }

    pub fn apply_key(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "host" | "docker_host" => {
                // Repeatable: each line adds a host (`name=endpoint` names it). Empty clears.
                if value.is_empty() {
                    self.hosts.clear();
                } else {
                    self.hosts.push(HostRequest::parse(value));
                }
            }
            "context" => {
                if value.is_empty() {
                    self.hosts.retain(|h| h.context.is_none());
                } else {
                    self.hosts.push(HostRequest {
                        context: Some(value.into()),
                        ..Default::default()
                    });
                }
            }
            "all_contexts" => self.all_contexts = parse_bool(value, key)?,
            "interval" | "refresh_interval" => self.refresh_interval = parse_interval(value)?,
            "theme" => {
                if value.is_empty() {
                    return err("theme must not be empty");
                }
                self.theme_name = value.into();
            }
            "color" | "color_mode" => {
                if value.eq_ignore_ascii_case("auto") {
                    self.color_mode = None;
                } else {
                    self.color_mode = Some(ColorMode::parse(value).ok_or_else(|| {
                        ConfigurationError(format!(
                            "invalid color mode '{value}' — use auto, truecolor, 256, 16 or none"
                        ))
                    })?);
                }
            }
            "mouse" => self.mouse = parse_bool(value, key)?,
            "gpu" => self.gpu = parse_bool(value, key)?,
            "show_stopped" | "all" => self.show_stopped = parse_bool(value, key)?,
            "sort" => {
                self.sort_key = SortKey::parse(&value.to_lowercase()).ok_or_else(|| {
                    let all: Vec<&str> = SortKey::ALL.iter().map(|k| k.raw_value()).collect();
                    ConfigurationError(format!(
                        "invalid sort '{value}' — use one of {}",
                        all.join(", ")
                    ))
                })?;
            }
            "sort_descending" | "descending" => {
                self.sort_descending = Some(parse_bool(value, key)?)
            }
            "history" | "history_size" => match value.parse::<usize>() {
                Ok(size) if (HISTORY_RANGE.0..=HISTORY_RANGE.1).contains(&size) => {
                    self.history_size = size
                }
                _ => {
                    return err(format!(
                        "history must be a whole number between {} and {}",
                        HISTORY_RANGE.0, HISTORY_RANGE.1
                    ))
                }
            },
            _ => {
                if !apply_tls_key(key, value, &mut self.global_tls)? {
                    return err(format!("unknown setting '{key}'"));
                }
            }
        }
        Ok(())
    }

    /// Pre-scans for `--config` so the file applies before other flags override it.
    pub fn config_path_argument(arguments: &[String]) -> Result<Option<String>> {
        let mut index = 0;
        while index < arguments.len() {
            let argument = &arguments[index];
            if argument == "-c" || argument == "--config" {
                return match arguments.get(index + 1) {
                    Some(path) => Ok(Some(path.clone())),
                    None => err(format!("{argument} requires a file path")),
                };
            }
            if let Some(value) = argument.strip_prefix("--config=") {
                return Ok(Some(value.into()));
            }
            index += 1;
        }
        Ok(None)
    }

    /// Applies command-line flags (without the program name).
    pub fn apply_arguments(&mut self, arguments: &[String]) -> Result<()> {
        let mut index = 0;
        while index < arguments.len() {
            let original = arguments[index].clone();
            let (argument, inline) = match original
                .strip_prefix("--")
                .and_then(|_| original.split_once('='))
            {
                Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
                None => (original.clone(), None),
            };
            let value = |index: &mut usize| -> Result<String> {
                if let Some(v) = &inline {
                    return Ok(v.clone());
                }
                *index += 1;
                arguments.get(*index).cloned().ok_or_else(|| {
                    ConfigurationError(format!("{argument} requires a value — see dtop --help"))
                })
            };
            match argument.as_str() {
                "-h" | "--help" => self.mode = RunMode::Help,
                "-v" | "--version" => self.mode = RunMode::Version,
                "--once" => self.mode = RunMode::Once,
                "--list-themes" => self.mode = RunMode::ListThemes,
                "--print-theme" => self.mode = RunMode::PrintTheme(value(&mut index)?),
                "-H" | "--host" => {
                    let v = value(&mut index)?;
                    self.start_command_line_hosts();
                    self.hosts.push(HostRequest::parse(&v));
                }
                "--context" => {
                    let v = value(&mut index)?;
                    self.start_command_line_hosts();
                    self.hosts.push(HostRequest {
                        context: Some(v),
                        ..Default::default()
                    });
                }
                "--all-contexts" => self.all_contexts = true,
                "--tls" => {
                    self.global_tls.enabled = Some(
                        inline
                            .as_deref()
                            .map_or(Ok(true), |v| parse_bool(v, "--tls"))?,
                    )
                }
                "--tlsverify" => {
                    self.global_tls.verify = Some(
                        inline
                            .as_deref()
                            .map_or(Ok(true), |v| parse_bool(v, "--tlsverify"))?,
                    )
                }
                "--tlscacert" => self.global_tls.ca_file = Some(value(&mut index)?),
                "--tlscert" => self.global_tls.certificate_file = Some(value(&mut index)?),
                "--tlskey" => self.global_tls.key_file = Some(value(&mut index)?),
                "-i" | "--interval" => self.refresh_interval = parse_interval(&value(&mut index)?)?,
                "-t" | "--theme" => self.theme_name = value(&mut index)?,
                "--color" => {
                    let v = value(&mut index)?;
                    self.apply_key("color", &v)?;
                }
                "--running" => self.show_stopped = false,
                "--all" => self.show_stopped = true,
                "--sort" => {
                    let v = value(&mut index)?;
                    self.apply_key("sort", &v)?;
                }
                "--no-mouse" => self.mouse = false,
                "--mouse" => self.mouse = true,
                "--no-gpu" => self.gpu = false,
                "--gpu" => self.gpu = true,
                "--history" => {
                    let v = value(&mut index)?;
                    self.apply_key("history", &v)?;
                }
                "-c" | "--config" => self.config_file_path = Some(value(&mut index)?),
                _ => {
                    return err(format!(
                        "unknown option '{original}' — run dtop --help for usage"
                    ))
                }
            }
            index += 1;
        }
        Ok(())
    }

    /// The first `--host`/`--context` on the command line replaces the file's hosts.
    fn start_command_line_hosts(&mut self) {
        if !self.hosts_from_command_line {
            self.hosts_from_command_line = true;
            self.hosts.clear();
        }
    }

    /// Where `dtop.conf` and `themes/` live: `$XDG_CONFIG_HOME/dtop`, `~/.config/dtop`, or
    /// `%APPDATA%\dtop` on Windows.
    pub fn config_directory(
        environment: &HashMap<String, String>,
        platform: Platform,
        home: Option<&str>,
    ) -> Option<String> {
        let get = |k: &str| environment.get(k).filter(|v| !v.is_empty()).cloned();
        if platform == Platform::Windows {
            if let Some(app_data) = get("APPDATA") {
                return Some(join_path(&[app_data.as_str(), "dtop"], platform));
            }
            return get("USERPROFILE")
                .map(|p| join_path(&[p.as_str(), "AppData", "Roaming", "dtop"], platform));
        }
        if let Some(xdg) = get("XDG_CONFIG_HOME").filter(|x| x.starts_with('/')) {
            return Some(join_path(&[xdg.as_str(), "dtop"], platform));
        }
        home.filter(|h| !h.is_empty())
            .map(|h| join_path(&[h, ".config", "dtop"], platform))
    }
}

/// Keys allowed inside a `[host NAME]` section.
fn apply_host_key(key: &str, value: &str, host: &mut HostRequest) -> Result<()> {
    let optional = (!value.is_empty()).then(|| value.to_string());
    match key {
        "endpoint" | "host" | "address" | "docker_host" => host.address = optional,
        "context" => host.context = optional,
        _ => {
            if !apply_tls_key(key, value, &mut host.tls)? {
                return err(format!(
                    "'{key}' is not valid in a [host] section — use endpoint, context, tls, tlsverify, tlscacert, tlscert, tlskey or cert_path"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_flags() {
        let mut c = AppConfiguration::default();
        c.apply_arguments(&args(&[
            "-i",
            "500ms",
            "--theme=ember",
            "--running",
            "--sort",
            "memory",
            "--no-gpu",
            "--tlsverify",
            "--host",
            "prod=tcp://p:2376",
        ]))
        .unwrap();
        assert!((c.refresh_interval - 0.5).abs() < 1e-9);
        assert_eq!(c.theme_name, "ember");
        assert!(!c.show_stopped && !c.gpu);
        assert_eq!(c.sort_key, SortKey::Memory);
        assert_eq!(c.global_tls.verify, Some(true));
        assert_eq!(c.hosts[0].name.as_deref(), Some("prod"));
        assert!(c.apply_arguments(&args(&["--bogus"])).is_err());
        assert!(c.apply_arguments(&args(&["-i", "100"])).is_err());
        assert!(c.apply_arguments(&args(&["--sort"])).is_err());
    }

    #[test]
    fn config_file_sections_and_command_line_precedence() {
        let mut c = AppConfiguration::default();
        c.apply_config_file("interval = 1\nhost = tcp://a:1\ntlsverify = true\n[host prod]\nendpoint = tcp://p:2376\ntlscacert = /c/ca.pem\n").unwrap();
        assert_eq!(c.hosts.len(), 2);
        assert_eq!(c.hosts[1].tls.ca_file.as_deref(), Some("/c/ca.pem"));
        assert_eq!(c.global_tls.verify, Some(true));
        c.apply_arguments(&args(&["--context", "x", "--context", "y"]))
            .unwrap();
        assert_eq!(
            c.hosts
                .iter()
                .filter_map(|h| h.context.as_deref())
                .collect::<Vec<_>>(),
            vec!["x", "y"]
        );
    }

    #[test]
    fn config_file_errors_name_the_line() {
        let mut c = AppConfiguration::default();
        let e = c.apply_config_file("\nbogus = 1").unwrap_err();
        assert!(e.0.starts_with("dtop.conf line 2"));
        assert!(AppConfiguration::default()
            .apply_config_file("[host x]\n")
            .is_err());
        assert!(AppConfiguration::default()
            .apply_config_file("[host x]\nendpoint = tcp://a:1\ncontext = c\n")
            .is_err());
        assert!(AppConfiguration::default()
            .apply_config_file("[group x]\n")
            .is_err());
    }

    #[test]
    fn finds_the_config_directory() {
        let env: HashMap<String, String> =
            [("XDG_CONFIG_HOME".to_string(), "/x".to_string())].into();
        assert_eq!(
            AppConfiguration::config_directory(&env, Platform::Linux, Some("/h")).as_deref(),
            Some("/x/dtop")
        );
        assert_eq!(
            AppConfiguration::config_directory(&HashMap::new(), Platform::MacOS, Some("/h"))
                .as_deref(),
            Some("/h/.config/dtop")
        );
        let win: HashMap<String, String> = [("APPDATA".to_string(), r"C:\A".to_string())].into();
        assert_eq!(
            AppConfiguration::config_directory(&win, Platform::Windows, None).as_deref(),
            Some(r"C:\A\dtop")
        );
        assert_eq!(
            AppConfiguration::config_path_argument(&args(&["--config=/f"]))
                .unwrap()
                .as_deref(),
            Some("/f")
        );
    }
}
