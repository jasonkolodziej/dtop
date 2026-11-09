//! Process entry: configuration, host resolution, then `--once` or the interactive UI.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use dtop_core::gpu::SystemGpuCollector;
use dtop_core::host::{home_directory, resolve_hosts, HostEnvironment};
use dtop_core::platform::{join_path, Platform};
use dtop_core::tls::client_for_host;
use dtop_core::DockerApi;
use dtop_ui::theme::serialize_theme;
use dtop_ui::{ColorMode, Terminal, TerminalError};

use crate::config::{AppConfiguration, ConfigurationError, RunMode, USAGE};
use crate::controller::{AppController, ControllerOptions, HostConnection};
use crate::once;
use crate::refresher::{DataRefresher, RefresherOptions};
use crate::state::{ConnectionState, APP_NAME, APP_VERSION};
use crate::theme_files::{ThemeLoader, ThemeStore};

fn read_text_file(path: &str) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn list_directory(path: &str) -> Vec<String> {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn print_error(message: &str) {
    let _ = writeln!(std::io::stderr(), "{message}");
}

/// Runs dtop with `arguments` (without the program name); returns the exit status.
pub fn run(arguments: &[String]) -> i32 {
    let environment: HashMap<String, String> = std::env::vars().collect();
    let platform = Platform::current();
    let home = home_directory();
    let config_directory =
        AppConfiguration::config_directory(&environment, platform, home.as_deref());
    let mut configuration = AppConfiguration::default();
    let explicit_config = match load_configuration(
        &mut configuration,
        arguments,
        config_directory.as_deref(),
        platform,
    ) {
        Ok(path) => path,
        Err(error) => {
            print_error(&format!("dtop: {error}"));
            return 2;
        }
    };
    let theme_loader = ThemeLoader {
        themes_directory: config_directory
            .as_ref()
            .map(|d| join_path(&[d.as_str(), "themes"], platform)),
        platform,
    };
    match &configuration.mode {
        RunMode::Help => {
            println!("{USAGE}");
            return 0;
        }
        RunMode::Version => {
            println!("{APP_NAME} {APP_VERSION}");
            return 0;
        }
        RunMode::ListThemes => {
            for name in theme_loader.available_names(&list_directory) {
                println!("{name}");
            }
            return 0;
        }
        RunMode::PrintTheme(name) => {
            return match theme_loader.load(name, &read_text_file) {
                Ok(theme) => {
                    print!("{}", serialize_theme(&theme));
                    0
                }
                Err(error) => {
                    print_error(&format!("dtop: {error}"));
                    2
                }
            };
        }
        RunMode::Once | RunMode::Interactive => {}
    }
    let host_environment = HostEnvironment::system();
    let (connections, warnings) =
        match connect(&configuration, &host_environment, &environment, platform) {
            Ok(result) => result,
            Err(error) => {
                print_error(&format!("dtop: {error}"));
                return 2;
            }
        };
    if configuration.mode == RunMode::Once {
        for warning in &warnings {
            print_error(&format!("dtop: skipped {warning}"));
        }
        return run_once(&connections, &environment);
    }
    if !Terminal::is_interactive() {
        print_error(&format!("dtop: {}", TerminalError::NotATerminal));
        return 2;
    }
    let theme = match theme_loader.load(&configuration.theme_name, &read_text_file) {
        Ok(theme) => theme,
        Err(error) => {
            print_error(&format!("dtop: {error}"));
            return 2;
        }
    };
    let color_mode = configuration
        .color_mode
        .unwrap_or_else(|| ColorMode::detect(&environment));
    let theme_names = theme_loader.available_names(&list_directory);
    let controller = AppController::new(ControllerOptions {
        configuration,
        hosts: connections,
        notice: (!warnings.is_empty()).then(|| format!("Skipped {}", warnings.join("; "))),
        theme,
        color_mode,
        theme_loader,
        theme_names,
        theme_store: ThemeStore::new(config_directory, explicit_config, platform),
        environment,
    });
    match controller.run() {
        Ok(()) => 0,
        Err(error) => {
            print_error(&format!("dtop: {error}"));
            1
        }
    }
}

/// Applies the config file (explicit, else the default location) and then the flags. Returns
/// the explicit config path, if any.
fn load_configuration(
    configuration: &mut AppConfiguration,
    arguments: &[String],
    config_directory: Option<&str>,
    platform: Platform,
) -> Result<Option<String>, ConfigurationError> {
    let explicit = AppConfiguration::config_path_argument(arguments)?;
    let default = config_directory.map(|d| join_path(&[d, "dtop.conf"], platform));
    if let Some(path) = explicit.clone().or(default) {
        match read_text_file(&path) {
            Some(text) => configuration.apply_config_file(&text)?,
            None if explicit.is_some() => {
                return Err(ConfigurationError(format!(
                    "config file '{path}' could not be read — check the path and permissions"
                )))
            }
            None => {}
        }
    }
    configuration.apply_arguments(arguments)?;
    Ok(explicit)
}

type Connections = (Vec<HostConnection>, Vec<String>);

fn connect(
    configuration: &AppConfiguration,
    host_environment: &HostEnvironment,
    environment: &HashMap<String, String>,
    platform: Platform,
) -> Result<Connections, String> {
    let resolved = resolve_hosts(
        &configuration.hosts,
        &configuration.global_tls,
        configuration.all_contexts,
        host_environment,
    )
    .map_err(|e| e.to_string())?;
    if resolved.hosts.is_empty() {
        return Err("no Docker hosts to monitor — pass --host or --context".into());
    }
    let path = environment
        .get("PATH")
        .or_else(|| environment.get("Path"))
        .cloned()
        .unwrap_or_default();
    let gpu = if configuration.gpu {
        SystemGpuCollector::make_default(&path, platform)
    } else {
        None
    };
    // GPU usage is measured on this machine, so it belongs to the first local daemon.
    let gpu_host = resolved.hosts.iter().position(|h| h.endpoint.is_local());
    let mut connections = Vec::new();
    for (index, host) in resolved.hosts.into_iter().enumerate() {
        let client = client_for_host(&host, 30).map_err(|e| e.to_string())?;
        let api: Arc<dyn DockerApi> = Arc::new(client);
        let options = RefresherOptions {
            // Compose commands name the host unless it is the docker CLI's own default.
            host: (host.source != "default socket").then(|| host.clone()),
            gpu_collector: if Some(index) == gpu_host {
                gpu.clone()
            } else {
                None
            },
            history_size: configuration.history_size,
            ..RefresherOptions::default()
        };
        let refresher = Arc::new(DataRefresher::new(
            Arc::clone(&api),
            &host.description(),
            options,
        ));
        connections.push(HostConnection {
            host,
            api,
            refresher,
        });
    }
    Ok((connections, resolved.warnings))
}

/// Two refreshes one second apart so CPU and I/O rates have a baseline; every host in parallel.
/// Exit status 1 if any host could not be reached.
fn run_once(hosts: &[HostConnection], environment: &HashMap<String, String>) -> i32 {
    let snapshots: Vec<_> = std::thread::scope(|scope| {
        #[expect(
            clippy::needless_collect,
            reason = "spawn every host before joining any, so they poll in parallel"
        )]
        let handles: Vec<_> = hosts
            .iter()
            .map(|h| {
                scope.spawn(move || {
                    let mut snapshot = h.refresher.refresh(true);
                    if snapshot.connection.is_connected() {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        snapshot = h.refresher.refresh(false);
                    }
                    snapshot
                })
            })
            .collect();
        handles
            .into_iter()
            .zip(hosts)
            .map(|(handle, h)| handle.join().unwrap_or_else(|_| h.refresher.snapshot()))
            .collect()
    });
    let width = environment
        .get("COLUMNS")
        .and_then(|c| c.parse().ok())
        .unwrap_or(120usize)
        .max(40);
    let named: Vec<(String, _)> = hosts
        .iter()
        .map(|h| h.host.name.clone())
        .zip(snapshots)
        .collect();
    let report = once::render_hosts(&named, width);
    let failed = named
        .iter()
        .any(|(_, s)| matches!(s.connection, ConnectionState::Disconnected { .. }));
    if failed && named.len() == 1 {
        print_error(report.trim_end_matches('\n'));
        return 1;
    }
    print!("{report}");
    let _ = std::io::stdout().flush();
    i32::from(failed)
}
