//! Compose projects reconstructed from container labels, and compose operations. Operations the
//! Engine API can express run container by container; up/down/pull shell out to
//! `docker compose` with the working directory and files recorded in the labels.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crate::api::ContainerAction;
use crate::endpoint::DockerEndpoint;
use crate::error::{DockerError, Result};
use crate::host::DockerHost;
use crate::models::{Container, ContainerState, HealthStatus};
use crate::platform::{join_path, Platform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComposeOperation {
    Up,
    Down,
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Pull,
}

impl ComposeOperation {
    pub const ALL: [ComposeOperation; 8] = [
        ComposeOperation::Up,
        ComposeOperation::Down,
        ComposeOperation::Start,
        ComposeOperation::Stop,
        ComposeOperation::Restart,
        ComposeOperation::Pause,
        ComposeOperation::Unpause,
        ComposeOperation::Pull,
    ];

    pub fn raw_value(self) -> &'static str {
        match self {
            ComposeOperation::Up => "up",
            ComposeOperation::Down => "down",
            ComposeOperation::Start => "start",
            ComposeOperation::Stop => "stop",
            ComposeOperation::Restart => "restart",
            ComposeOperation::Pause => "pause",
            ComposeOperation::Unpause => "unpause",
            ComposeOperation::Pull => "pull",
        }
    }

    /// Whether it can run against the Engine API without the compose CLI.
    pub fn is_engine_native(self) -> bool {
        !matches!(
            self,
            ComposeOperation::Up | ComposeOperation::Down | ComposeOperation::Pull
        )
    }

    pub fn is_destructive(self) -> bool {
        self == ComposeOperation::Down
    }

    /// The equivalent per-container action.
    pub fn container_action(self, stop_timeout: Option<i64>) -> Option<ContainerAction> {
        match self {
            ComposeOperation::Start => Some(ContainerAction::Start),
            ComposeOperation::Stop => Some(ContainerAction::Stop {
                timeout_seconds: stop_timeout,
            }),
            ComposeOperation::Restart => Some(ContainerAction::Restart {
                timeout_seconds: stop_timeout,
            }),
            ComposeOperation::Pause => Some(ContainerAction::Pause),
            ComposeOperation::Unpause => Some(ContainerAction::Unpause),
            _ => None,
        }
    }

    /// Which containers the per-container action applies to.
    pub fn applies_to(self, container: &Container) -> bool {
        use ContainerState::*;
        match self {
            ComposeOperation::Start => matches!(container.state, Exited | Created | Dead),
            ComposeOperation::Stop => matches!(container.state, Running | Restarting | Paused),
            ComposeOperation::Restart => container.state != Removing,
            ComposeOperation::Pause => container.state == Running,
            ComposeOperation::Unpause => container.state == Paused,
            _ => false,
        }
    }
}

/// A fully specified external command, kept as data so construction is testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInvocation {
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: Option<String>,
    /// Environment variables to remove from this child process.
    pub environment_remove: Vec<String>,
}

impl CommandInvocation {
    /// Shell-quoted rendering for display (never executed through a shell).
    pub fn display_string(&self) -> String {
        std::iter::once(&self.executable)
            .chain(&self.arguments)
            .map(|a| quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub fn quote(argument: &str) -> String {
    let safe = argument
        .chars()
        .all(|c| c.is_alphanumeric() || "-_./:=@,+".contains(c));
    if safe && !argument.is_empty() {
        argument.to_string()
    } else {
        format!("'{}'", argument.replace('\'', r"'\''"))
    }
}

/// Builds `docker [host flags] compose --project-name <n> [--project-directory d] [--file f]... <op> [flags]`.
pub fn compose_invocation(
    operation: ComposeOperation,
    project: &ComposeProject,
    docker_executable: &str,
    endpoint: Option<&DockerEndpoint>,
    host: Option<&DockerHost>,
) -> CommandInvocation {
    let mut arguments: Vec<String> = Vec::new();
    if let Some(host) = host {
        arguments.extend(host.docker_cli_arguments());
    } else if let Some(endpoint) = endpoint {
        arguments.extend(["--host".to_string(), endpoint.to_string()]);
    }
    arguments.extend([
        "compose".into(),
        "--project-name".into(),
        project.name.clone(),
    ]);
    if let Some(directory) = project.working_directory.as_ref().filter(|d| !d.is_empty()) {
        arguments.extend(["--project-directory".into(), directory.clone()]);
    }
    for file in &project.config_files {
        arguments.extend(["--file".into(), file.clone()]);
    }
    arguments.push(operation.raw_value().into());
    match operation {
        ComposeOperation::Up => arguments.push("--detach".into()),
        ComposeOperation::Down => arguments.push("--remove-orphans".into()),
        _ => {}
    }
    CommandInvocation {
        executable: docker_executable.into(),
        arguments,
        working_directory: project.working_directory.clone(),
        environment_remove: if host.is_some() || endpoint.is_some() {
            [
                "DOCKER_HOST",
                "DOCKER_CONTEXT",
                "DOCKER_TLS",
                "DOCKER_TLS_VERIFY",
                "DOCKER_CERT_PATH",
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        } else {
            Vec::new()
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub exit_code: i32,
    pub output: String,
}

impl CommandResult {
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }
}

/// Runs external commands; a trait so the app is testable without spawning processes.
pub trait CommandRunning: Send + Sync {
    fn run(&self, invocation: &CommandInvocation) -> Result<CommandResult>;

    /// Like `run`, but a command still running after `timeout` is killed and reported as
    /// `DockerError::TimedOut`, so a wedged tool (such as `nvidia-smi` on a hung driver) cannot
    /// stall its caller. Runners without a clock (test fakes) just run the command.
    fn run_with_timeout(
        &self,
        invocation: &CommandInvocation,
        timeout: Duration,
    ) -> Result<CommandResult> {
        let _ = timeout;
        self.run(invocation)
    }
}

/// Spawns processes with `std::process::Command`, resolving the executable on `PATH`.
pub struct ProcessCommandRunner {
    pub path: String,
    pub platform: Platform,
}

impl Default for ProcessCommandRunner {
    fn default() -> Self {
        let path = std::env::var("PATH")
            .or_else(|_| std::env::var("Path"))
            .unwrap_or_default();
        ProcessCommandRunner {
            path,
            platform: Platform::current(),
        }
    }
}

impl ProcessCommandRunner {
    fn command(&self, invocation: &CommandInvocation) -> Result<std::process::Command> {
        let executable = resolve_executable(&invocation.executable, &self.path, self.platform, &|p| {
            std::path::Path::new(p).is_file()
        })
        .ok_or_else(|| {
            DockerError::Io(format!(
                "'{}' was not found on PATH — install the Docker CLI with the compose plugin to use compose up/down/pull",
                invocation.executable
            ))
        })?;
        let mut command = std::process::Command::new(executable);
        command
            .args(&invocation.arguments)
            .stdin(std::process::Stdio::null());
        for variable in &invocation.environment_remove {
            command.env_remove(variable);
        }
        if let Some(directory) = invocation
            .working_directory
            .as_ref()
            .filter(|d| std::path::Path::new(d).exists())
        {
            command.current_dir(directory);
        }
        Ok(command)
    }
}

fn launch_failed(invocation: &CommandInvocation, error: &std::io::Error) -> DockerError {
    DockerError::Io(format!(
        "failed to launch {}: {error}",
        invocation.display_string()
    ))
}

/// Reads a child's pipe to the end on its own thread, so a full pipe never blocks the child.
fn drain<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> Option<std::thread::JoinHandle<Vec<u8>>> {
    let mut pipe = pipe?;
    std::thread::Builder::new()
        .name("dtop-command-output".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
        .ok()
}

impl CommandRunning for ProcessCommandRunner {
    fn run(&self, invocation: &CommandInvocation) -> Result<CommandResult> {
        let output = self
            .command(invocation)?
            .output()
            .map_err(|e| launch_failed(invocation, &e))?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok(CommandResult {
            exit_code: output.status.code().unwrap_or(-1),
            output: text,
        })
    }

    fn run_with_timeout(
        &self,
        invocation: &CommandInvocation,
        timeout: Duration,
    ) -> Result<CommandResult> {
        use std::process::Stdio;
        let mut child = self
            .command(invocation)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| launch_failed(invocation, &e))?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    // The output threads end once the killed child's pipes close; they are not
                    // joined, in case a grandchild still holds a pipe open.
                    return Err(DockerError::TimedOut(format!(
                        "{} (stopped after {} s)",
                        invocation.display_string(),
                        timeout.as_secs_f64()
                    )));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(DockerError::Io(format!(
                        "waiting for {} failed: {e}",
                        invocation.display_string()
                    )));
                }
            }
        };
        let collect = |handle: Option<std::thread::JoinHandle<Vec<u8>>>| {
            handle.and_then(|h| h.join().ok()).unwrap_or_default()
        };
        let mut text = String::from_utf8_lossy(&collect(stdout)).into_owned();
        text.push_str(&String::from_utf8_lossy(&collect(stderr)));
        Ok(CommandResult {
            exit_code: status.code().unwrap_or(-1),
            output: text,
        })
    }
}

/// Finds an executable on PATH (adding `.exe` on Windows).
pub fn resolve_executable(
    name: &str,
    path: &str,
    platform: Platform,
    file_exists: &dyn Fn(&str) -> bool,
) -> Option<String> {
    if name.contains('/') || (platform == Platform::Windows && name.contains('\\')) {
        return file_exists(name).then(|| name.to_string());
    }
    let separator = if platform == Platform::Windows {
        ';'
    } else {
        ':'
    };
    let suffixes: &[&str] =
        if platform == Platform::Windows && !name.to_lowercase().ends_with(".exe") {
            &[".exe", ""]
        } else {
            &[""]
        };
    for directory in path.split(separator).filter(|d| !d.is_empty()) {
        for suffix in suffixes {
            let candidate = join_path(&[directory, &format!("{name}{suffix}")], platform);
            if file_exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Roll-up status for a set of containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeStatus {
    Running,
    Partial,
    Degraded,
    Paused,
    Stopped,
    Empty,
}

impl ComposeStatus {
    pub fn raw_value(self) -> &'static str {
        match self {
            ComposeStatus::Running => "running",
            ComposeStatus::Partial => "partial",
            ComposeStatus::Degraded => "degraded",
            ComposeStatus::Paused => "paused",
            ComposeStatus::Stopped => "stopped",
            ComposeStatus::Empty => "empty",
        }
    }

    pub fn aggregate<'a>(containers: impl IntoIterator<Item = &'a Container>) -> ComposeStatus {
        let containers: Vec<&Container> = containers.into_iter().collect();
        if containers.is_empty() {
            return ComposeStatus::Empty;
        }
        let degraded = containers.iter().any(|c| {
            matches!(c.state, ContainerState::Restarting | ContainerState::Dead)
                || c.health() == Some(HealthStatus::Unhealthy)
        });
        if degraded {
            return ComposeStatus::Degraded;
        }
        let running = containers
            .iter()
            .filter(|c| c.state == ContainerState::Running)
            .count();
        let any_paused = containers.iter().any(|c| c.state == ContainerState::Paused);
        if running == containers.len() {
            ComposeStatus::Running
        } else if containers.iter().all(|c| c.state == ContainerState::Paused) {
            ComposeStatus::Paused
        } else if running == 0 && !any_paused {
            ComposeStatus::Stopped
        } else {
            ComposeStatus::Partial
        }
    }
}

/// A service: one or more replica containers sharing a service label.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposeService {
    pub project: String,
    pub name: String,
    pub containers: Vec<Container>,
}

impl ComposeService {
    pub fn id(&self) -> String {
        format!("{}/{}", self.project, self.name)
    }

    pub fn running_count(&self) -> usize {
        self.containers
            .iter()
            .filter(|c| c.state == ContainerState::Running)
            .count()
    }

    /// Worst state across replicas, so one crashing replica shows at the service level.
    pub fn aggregate_status(&self) -> ComposeStatus {
        ComposeStatus::aggregate(&self.containers)
    }
}

/// A project reconstructed from labels (the Engine API has no project object).
#[derive(Debug, Clone, PartialEq)]
pub struct ComposeProject {
    pub name: String,
    pub working_directory: Option<String>,
    pub config_files: Vec<String>,
    pub services: Vec<ComposeService>,
}

impl ComposeProject {
    pub fn containers(&self) -> impl Iterator<Item = &Container> {
        self.services.iter().flat_map(|s| &s.containers)
    }

    pub fn running_count(&self) -> usize {
        self.containers()
            .filter(|c| c.state == ContainerState::Running)
            .count()
    }

    pub fn aggregate_status(&self) -> ComposeStatus {
        ComposeStatus::aggregate(self.containers())
    }

    /// Groups containers into projects and services; standalone containers come back
    /// separately. Projects, services and replicas are sorted for a stable display.
    pub fn group(containers: &[Container]) -> (Vec<ComposeProject>, Vec<Container>) {
        let mut by_project: BTreeMap<String, Vec<&Container>> = BTreeMap::new();
        let mut standalone: Vec<Container> = Vec::new();
        for container in containers {
            match container.compose_project().filter(|p| !p.is_empty()) {
                Some(project) => by_project
                    .entry(project.to_string())
                    .or_default()
                    .push(container),
                None => standalone.push(container.clone()),
            }
        }
        let projects = by_project
            .into_iter()
            .map(|(name, members)| {
                let mut by_service: BTreeMap<String, Vec<Container>> = BTreeMap::new();
                for c in &members {
                    by_service
                        .entry(c.compose_service().unwrap_or("(unknown)").to_string())
                        .or_default()
                        .push((*c).clone());
                }
                let services = by_service
                    .into_iter()
                    .map(|(service, mut replicas)| {
                        replicas.sort_by(|a, b| {
                            let left = a.compose_container_number().unwrap_or(i64::MAX);
                            let right = b.compose_container_number().unwrap_or(i64::MAX);
                            left.cmp(&right).then_with(|| a.name().cmp(&b.name()))
                        });
                        ComposeService {
                            project: name.clone(),
                            name: service,
                            containers: replicas,
                        }
                    })
                    .collect();
                // Containers may come from different directories after a move; the most
                // common value wins so one stale container does not.
                let working_directory =
                    most_common(members.iter().filter_map(|c| c.compose_working_directory()));
                let config_files = members
                    .iter()
                    .find(|c| c.compose_working_directory() == working_directory.as_deref())
                    .or_else(|| members.first())
                    .map(|c| c.compose_config_files())
                    .unwrap_or_default();
                ComposeProject {
                    name,
                    working_directory,
                    config_files,
                    services,
                }
            })
            .collect();
        standalone.sort_by_key(Container::name);
        (projects, standalone)
    }
}

fn most_common<'a>(values: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
    // Highest count; ties go to the lexicographically smallest value.
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(v, _)| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::compose_label;

    fn member(
        name: &str,
        project: &str,
        service: &str,
        number: &str,
        state: ContainerState,
    ) -> Container {
        let mut c = Container::new(name, name, "img", state);
        c.labels
            .insert(compose_label::PROJECT.into(), project.into());
        c.labels
            .insert(compose_label::SERVICE.into(), service.into());
        c.labels
            .insert(compose_label::CONTAINER_NUMBER.into(), number.into());
        c.labels
            .insert(compose_label::WORKING_DIR.into(), "/srv/app".into());
        c.labels.insert(
            compose_label::CONFIG_FILES.into(),
            "/srv/app/compose.yaml, /srv/app/o.yaml".into(),
        );
        c
    }

    #[test]
    fn groups_projects_services_and_replicas() {
        let containers = vec![
            member("web-2", "app", "web", "2", ContainerState::Running),
            member("web-1", "app", "web", "1", ContainerState::Running),
            member("db-1", "app", "db", "1", ContainerState::Exited),
            Container::new("solo", "solo", "img", ContainerState::Running),
        ];
        let (projects, standalone) = ComposeProject::group(&containers);
        assert_eq!(projects.len(), 1);
        let project = &projects[0];
        assert_eq!(
            project
                .services
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["db", "web"]
        );
        assert_eq!(project.services[1].containers[0].name(), "web-1");
        assert_eq!(project.working_directory.as_deref(), Some("/srv/app"));
        assert_eq!(
            project.config_files,
            vec!["/srv/app/compose.yaml", "/srv/app/o.yaml"]
        );
        assert_eq!(project.aggregate_status(), ComposeStatus::Partial);
        assert_eq!(standalone.len(), 1);
    }

    #[test]
    fn aggregates_status() {
        let running = Container::new("a", "a", "i", ContainerState::Running);
        let paused = Container::new("b", "b", "i", ContainerState::Paused);
        let dead = Container::new("c", "c", "i", ContainerState::Dead);
        assert_eq!(ComposeStatus::aggregate([&running]), ComposeStatus::Running);
        assert_eq!(ComposeStatus::aggregate([&paused]), ComposeStatus::Paused);
        assert_eq!(
            ComposeStatus::aggregate([&running, &dead]),
            ComposeStatus::Degraded
        );
        assert_eq!(
            ComposeStatus::aggregate(Vec::<&Container>::new()),
            ComposeStatus::Empty
        );
    }

    #[test]
    fn builds_compose_invocations_with_host_flags() {
        let project = ComposeProject {
            name: "app".into(),
            working_directory: Some("/srv/app".into()),
            config_files: vec!["/srv/app/compose.yaml".into()],
            services: Vec::new(),
        };
        let mut host = DockerHost::new(
            "prod",
            DockerEndpoint::Tcp {
                host: "prod".into(),
                port: 2376,
            },
        );
        host.tls = Some(crate::host::TlsConfiguration {
            verify_peer: true,
            ca_file: Some("/c/ca.pem".into()),
            ..Default::default()
        });
        let inv = compose_invocation(ComposeOperation::Up, &project, "docker", None, Some(&host));
        assert_eq!(
            inv.display_string(),
            "docker --host tcp://prod:2376 --tlsverify --tlscacert /c/ca.pem compose --project-name app \
             --project-directory /srv/app --file /srv/app/compose.yaml up --detach"
        );
        assert_eq!(
            inv.environment_remove,
            [
                "DOCKER_HOST",
                "DOCKER_CONTEXT",
                "DOCKER_TLS",
                "DOCKER_TLS_VERIFY",
                "DOCKER_CERT_PATH",
            ]
        );
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[cfg(unix)]
    #[test]
    fn process_runner_removes_requested_environment_only_from_the_child() {
        let invocation = CommandInvocation {
            executable: "/usr/bin/env".into(),
            arguments: Vec::new(),
            working_directory: None,
            environment_remove: vec!["HOME".into()],
        };
        let result = ProcessCommandRunner::default().run(&invocation).unwrap();
        assert!(result.succeeded());
        assert!(!result.output.lines().any(|line| line.starts_with("HOME=")));
    }

    #[test]
    fn resolves_executables_on_path() {
        let exists = |p: &str| p == "/usr/bin/docker" || p == r"C:\bin\docker.exe";
        assert_eq!(
            resolve_executable("docker", "/bin:/usr/bin", Platform::Linux, &exists).as_deref(),
            Some("/usr/bin/docker")
        );
        assert_eq!(
            resolve_executable("docker", r"C:\bin", Platform::Windows, &exists).as_deref(),
            Some(r"C:\bin\docker.exe")
        );
        assert_eq!(
            resolve_executable("nope", "/bin", Platform::Linux, &exists),
            None
        );
    }

    fn invocation(executable: &str, arguments: &[&str]) -> CommandInvocation {
        CommandInvocation {
            executable: executable.into(),
            arguments: arguments.iter().map(|a| a.to_string()).collect(),
            working_directory: None,
            environment_remove: Vec::new(),
        }
    }

    #[test]
    fn commands_that_overrun_their_timeout_are_killed() {
        // About 30 s if left alone.
        let slow = if cfg!(windows) {
            invocation("ping", &["-n", "30", "127.0.0.1"])
        } else {
            invocation("sleep", &["30"])
        };
        let started = Instant::now();
        let outcome =
            ProcessCommandRunner::default().run_with_timeout(&slow, Duration::from_millis(300));
        assert!(
            matches!(outcome, Err(DockerError::TimedOut(_))),
            "{outcome:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn commands_within_their_timeout_return_all_output() {
        let quick = if cfg!(windows) {
            invocation("cmd", &["/C", "echo out & echo err 1>&2 & exit 3"])
        } else {
            invocation("sh", &["-c", "echo out; echo err >&2; exit 3"])
        };
        let result = ProcessCommandRunner::default()
            .run_with_timeout(&quick, Duration::from_secs(20))
            .unwrap();
        assert_eq!(result.exit_code, 3);
        assert!(result.output.contains("out") && result.output.contains("err"));
    }
}
