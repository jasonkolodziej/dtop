//! Docker Engine API models. Decoding is tolerant: Docker omits fields, sends `null` and changes
//! types between API versions, so missing or mistyped fields fall back to defaults. Only a
//! missing identity (`Id`, `Name`) fails a decode.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{DockerError, Result};
use crate::json::Value;

fn to_map(pairs: Vec<(String, String)>) -> BTreeMap<String, String> {
    pairs.into_iter().collect()
}

fn require_str(value: &Value, key: &str, model: &str) -> Result<String> {
    value
        .get(key)
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| DockerError::Decoding(format!("{model}: missing '{key}'")))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ContainerState {
    Created,
    Running,
    Paused,
    Restarting,
    Removing,
    Exited,
    Dead,
    Unknown(String),
}

impl ContainerState {
    pub fn from_raw(raw: &str) -> Self {
        match raw.to_lowercase().as_str() {
            "created" => ContainerState::Created,
            "running" => ContainerState::Running,
            "paused" => ContainerState::Paused,
            "restarting" => ContainerState::Restarting,
            "removing" => ContainerState::Removing,
            "exited" => ContainerState::Exited,
            "dead" => ContainerState::Dead,
            _ => ContainerState::Unknown(raw.to_string()),
        }
    }

    pub fn raw_value(&self) -> &str {
        match self {
            ContainerState::Created => "created",
            ContainerState::Running => "running",
            ContainerState::Paused => "paused",
            ContainerState::Restarting => "restarting",
            ContainerState::Removing => "removing",
            ContainerState::Exited => "exited",
            ContainerState::Dead => "dead",
            ContainerState::Unknown(value) => value,
        }
    }

    /// Whether the container has live processes (stats and `top` are meaningful).
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            ContainerState::Running | ContainerState::Restarting | ContainerState::Paused
        )
    }

    /// Sort rank so running containers float to the top of status-sorted lists.
    pub fn sort_rank(&self) -> u8 {
        match self {
            ContainerState::Running => 0,
            ContainerState::Restarting => 1,
            ContainerState::Paused => 2,
            ContainerState::Created => 3,
            ContainerState::Removing => 4,
            ContainerState::Exited => 5,
            ContainerState::Dead => 6,
            ContainerState::Unknown(_) => 7,
        }
    }
}

impl std::fmt::Display for ContainerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.raw_value())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HealthStatus {
    Starting,
    Healthy,
    Unhealthy,
    None,
}

impl HealthStatus {
    pub fn from_raw(raw: &str) -> Self {
        match raw.to_lowercase().as_str() {
            "starting" | "health: starting" => HealthStatus::Starting,
            "healthy" => HealthStatus::Healthy,
            "unhealthy" => HealthStatus::Unhealthy,
            _ => HealthStatus::None,
        }
    }

    pub fn raw_value(self) -> &'static str {
        match self {
            HealthStatus::Starting => "starting",
            HealthStatus::Healthy => "healthy",
            HealthStatus::Unhealthy => "unhealthy",
            HealthStatus::None => "none",
        }
    }

    /// Extracts the health suffix from a status string such as `Up 3 minutes (healthy)`.
    pub fn from_status_text(status: &str) -> Option<HealthStatus> {
        let open = status.rfind('(')?;
        let close = status.rfind(')')?;
        if open >= close {
            return None;
        }
        match HealthStatus::from_raw(&status[open + 1..close]) {
            HealthStatus::None => None,
            parsed => Some(parsed),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PortBinding {
    pub ip: Option<String>,
    pub private_port: i64,
    pub public_port: Option<i64>,
    pub kind: String,
}

impl PortBinding {
    pub fn new(private_port: i64, public_port: Option<i64>) -> Self {
        PortBinding {
            ip: None,
            private_port,
            public_port,
            kind: "tcp".into(),
        }
    }

    pub fn decode(v: &Value) -> Self {
        PortBinding {
            ip: v.opt_str("IP"),
            private_port: v.flexible_int("PrivatePort").unwrap_or(0),
            public_port: v.flexible_int("PublicPort"),
            kind: v.str_or("Type", "tcp"),
        }
    }

    /// Formatted like the Docker CLI: `0.0.0.0:8080->80/tcp` or `80/tcp`.
    pub fn display(&self) -> String {
        let Some(public) = self.public_port else {
            return format!("{}/{}", self.private_port, self.kind);
        };
        let host = match &self.ip {
            Some(ip) if ip.contains(':') => format!("[{ip}]"),
            Some(ip) => ip.clone(),
            None => "0.0.0.0".into(),
        };
        format!("{host}:{public}->{}/{}", self.private_port, self.kind)
    }
}

/// Well-known Docker Compose labels.
pub mod compose_label {
    pub const PROJECT: &str = "com.docker.compose.project";
    pub const SERVICE: &str = "com.docker.compose.service";
    pub const WORKING_DIR: &str = "com.docker.compose.project.working_dir";
    pub const CONFIG_FILES: &str = "com.docker.compose.project.config_files";
    pub const CONTAINER_NUMBER: &str = "com.docker.compose.container-number";
    pub const ONE_OFF: &str = "com.docker.compose.oneoff";
    pub const DEPENDS_ON: &str = "com.docker.compose.depends_on";
}

/// A row from `GET /containers/json`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Container {
    pub id: String,
    pub names: Vec<String>,
    pub image: String,
    pub image_id: String,
    pub command: String,
    pub created: i64,
    pub ports: Vec<PortBinding>,
    pub labels: BTreeMap<String, String>,
    pub state: ContainerState,
    pub status: String,
}

impl Container {
    pub fn new(id: &str, name: &str, image: &str, state: ContainerState) -> Self {
        Container {
            id: id.into(),
            names: vec![format!("/{name}")],
            image: image.into(),
            image_id: String::new(),
            command: String::new(),
            created: 0,
            ports: Vec::new(),
            labels: BTreeMap::new(),
            state,
            status: String::new(),
        }
    }

    pub fn decode(v: &Value) -> Result<Self> {
        Ok(Container {
            id: require_str(v, "Id", "Container")?,
            names: v.string_array("Names"),
            image: v.str_or("Image", ""),
            image_id: v.str_or("ImageID", ""),
            command: v.str_or("Command", ""),
            created: v.flexible_int("Created").unwrap_or(0),
            ports: v
                .get("Ports")
                .as_array()
                .map(|a| a.iter().map(PortBinding::decode).collect())
                .unwrap_or_default(),
            labels: to_map(v.string_map("Labels")),
            state: v.get("State").as_str().map_or(
                ContainerState::Unknown("unknown".into()),
                ContainerState::from_raw,
            ),
            status: v.str_or("Status", ""),
        })
    }

    pub fn short_id(&self) -> String {
        self.id.chars().take(12).collect()
    }

    /// Primary name without the leading slash Docker adds (the shortest of the names).
    pub fn name(&self) -> String {
        match self.names.iter().min_by_key(|n| n.chars().count()) {
            Some(first) => first.strip_prefix('/').unwrap_or(first).to_string(),
            None => self.short_id(),
        }
    }

    pub fn health(&self) -> Option<HealthStatus> {
        HealthStatus::from_status_text(&self.status)
    }

    pub fn label(&self, key: &str) -> Option<&str> {
        self.labels.get(key).map(String::as_str)
    }

    pub fn compose_project(&self) -> Option<&str> {
        self.label(compose_label::PROJECT)
    }

    pub fn compose_service(&self) -> Option<&str> {
        self.label(compose_label::SERVICE)
    }

    pub fn compose_working_directory(&self) -> Option<&str> {
        self.label(compose_label::WORKING_DIR)
    }

    pub fn compose_config_files(&self) -> Vec<String> {
        self.label(compose_label::CONFIG_FILES)
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn compose_container_number(&self) -> Option<i64> {
        self.label(compose_label::CONTAINER_NUMBER)
            .and_then(|n| n.parse().ok())
    }

    pub fn is_compose_one_off(&self) -> bool {
        self.label(compose_label::ONE_OFF)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }

    /// Distinct, sorted published ports for compact display.
    pub fn port_summary(&self) -> String {
        let unique: BTreeSet<String> = self.ports.iter().map(PortBinding::display).collect();
        unique.into_iter().collect::<Vec<_>>().join(", ")
    }
}

/// Raw payload of `GET /containers/{id}/stats`. Linux daemons fill cgroup counters; Windows
/// daemons fill the 100ns-tick fields.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContainerStats {
    pub read: String,
    pub preread: String,
    pub num_procs: i64,
    pub cpu: CpuStats,
    pub precpu: CpuStats,
    pub memory: MemoryStats,
    pub networks: BTreeMap<String, NetworkStats>,
    pub block_io: Vec<BlockIoEntry>,
    pub pids_current: Option<u64>,
    pub pids_limit: Option<u64>,
    /// Windows storage counters.
    pub storage_read_bytes: Option<u64>,
    pub storage_write_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CpuUsage {
    pub total_usage: u64,
    pub per_cpu_usage: Vec<u64>,
    pub kernel_mode: u64,
    pub user_mode: u64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CpuStats {
    pub usage: CpuUsage,
    pub system_usage: Option<u64>,
    pub online_cpus: Option<i64>,
}

impl CpuStats {
    /// CPUs to scale by: `online_cpus`, else the per-CPU array length (older kernels), else 1.
    pub fn effective_cpu_count(&self) -> i64 {
        if let Some(online) = self.online_cpus.filter(|n| *n > 0) {
            return online;
        }
        (self.usage.per_cpu_usage.iter().filter(|v| **v > 0).count() as i64).max(1)
    }

    fn decode(v: &Value) -> Self {
        let u = v.get("cpu_usage");
        let per_cpu = u
            .get("percpu_usage")
            .as_array()
            .and_then(|a| {
                a.iter()
                    .map(|x| {
                        if let Value::Number(n) = x {
                            n.as_u64()
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        CpuStats {
            usage: CpuUsage {
                total_usage: u.flexible_uint("total_usage").unwrap_or(0),
                per_cpu_usage: per_cpu,
                kernel_mode: u.flexible_uint("usage_in_kernelmode").unwrap_or(0),
                user_mode: u.flexible_uint("usage_in_usermode").unwrap_or(0),
            },
            system_usage: v.flexible_uint("system_cpu_usage"),
            online_cpus: v.flexible_int("online_cpus"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MemoryStats {
    pub usage: Option<u64>,
    pub limit: Option<u64>,
    pub max_usage: Option<u64>,
    pub stats: BTreeMap<String, u64>,
    /// Windows: private working set.
    pub private_working_set: Option<u64>,
    pub commit_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetworkStats {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BlockIoEntry {
    pub major: u64,
    pub minor: u64,
    pub op: String,
    pub value: u64,
}

impl ContainerStats {
    pub fn decode(v: &Value) -> Self {
        let m = v.get("memory_stats");
        let memory = if m.as_object().is_some() {
            let stats = m
                .get("stats")
                .as_object()
                .map(|members| {
                    members
                        .iter()
                        .filter_map(|(k, val)| match val {
                            Value::Number(n) => n.as_u64().map(|u| (k.clone(), u)),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            MemoryStats {
                usage: m.flexible_uint("usage"),
                limit: m.flexible_uint("limit"),
                max_usage: m.flexible_uint("max_usage"),
                stats,
                private_working_set: m.flexible_uint("privateworkingset"),
                commit_bytes: m.flexible_uint("commitbytes"),
            }
        } else {
            MemoryStats::default()
        };
        let networks = v
            .get("networks")
            .as_object()
            .map(|members| {
                members
                    .iter()
                    .filter(|(_, iface)| iface.as_object().is_some())
                    .map(|(name, iface)| {
                        (
                            name.clone(),
                            NetworkStats {
                                rx_bytes: iface.flexible_uint("rx_bytes").unwrap_or(0),
                                tx_bytes: iface.flexible_uint("tx_bytes").unwrap_or(0),
                                rx_packets: iface.flexible_uint("rx_packets").unwrap_or(0),
                                tx_packets: iface.flexible_uint("tx_packets").unwrap_or(0),
                                rx_errors: iface.flexible_uint("rx_errors").unwrap_or(0),
                                tx_errors: iface.flexible_uint("tx_errors").unwrap_or(0),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut block_io = Vec::new();
        if let Some(entries) = v
            .get("blkio_stats")
            .get("io_service_bytes_recursive")
            .as_array()
        {
            for entry in entries {
                if entry.as_object().is_none() {
                    break;
                }
                block_io.push(BlockIoEntry {
                    major: entry.flexible_uint("major").unwrap_or(0),
                    minor: entry.flexible_uint("minor").unwrap_or(0),
                    op: entry.str_or("op", ""),
                    value: entry.flexible_uint("value").unwrap_or(0),
                });
            }
        }
        let pids = v.get("pids_stats");
        let storage = v.get("storage_stats");
        ContainerStats {
            read: v.str_or("read", ""),
            preread: v.str_or("preread", ""),
            num_procs: v.flexible_int("num_procs").unwrap_or(0),
            cpu: CpuStats::decode(v.get("cpu_stats")),
            precpu: CpuStats::decode(v.get("precpu_stats")),
            memory,
            networks,
            block_io,
            pids_current: pids.flexible_uint("current"),
            pids_limit: pids.flexible_uint("limit"),
            storage_read_bytes: storage.flexible_uint("read_size_bytes"),
            storage_write_bytes: storage.flexible_uint("write_size_bytes"),
        }
    }

    /// Cumulative network bytes across every interface.
    pub fn total_network(&self) -> (u64, u64) {
        self.networks.values().fold((0u64, 0u64), |(rx, tx), i| {
            (rx.wrapping_add(i.rx_bytes), tx.wrapping_add(i.tx_bytes))
        })
    }

    /// Cumulative block I/O. cgroup v1 capitalizes ops ("Read"), v2 does not.
    pub fn total_block_io(&self) -> (u64, u64) {
        if self.block_io.is_empty()
            && (self.storage_read_bytes.is_some() || self.storage_write_bytes.is_some())
        {
            return (
                self.storage_read_bytes.unwrap_or(0),
                self.storage_write_bytes.unwrap_or(0),
            );
        }
        let mut read = 0u64;
        let mut write = 0u64;
        for entry in &self.block_io {
            match entry.op.to_lowercase().as_str() {
                "read" => read = read.wrapping_add(entry.value),
                "write" => write = write.wrapping_add(entry.value),
                _ => {}
            }
        }
        (read, write)
    }

    /// Memory in use excluding page cache, matching `docker stats`: cgroup v2 subtracts
    /// `inactive_file`, cgroup v1 the hierarchical `total_inactive_file` (or `cache`).
    pub fn memory_usage_excluding_cache(&self) -> u64 {
        if let (Some(ws), None) = (self.memory.private_working_set, self.memory.usage) {
            return ws;
        }
        let Some(usage) = self.memory.usage else {
            return 0;
        };
        let stats = &self.memory.stats;
        let cache = stats
            .get("total_inactive_file")
            .or_else(|| stats.get("inactive_file"))
            .or_else(|| stats.get("cache"))
            .copied()
            .unwrap_or(0);
        if cache < usage {
            usage - cache
        } else {
            usage
        }
    }
}

/// The subset of `GET /containers/{id}/json` the details panel renders.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContainerDetails {
    pub id: String,
    pub name: String,
    pub created: String,
    pub path: String,
    pub args: Vec<String>,
    pub state: DetailsState,
    pub image: String,
    pub image_name: String,
    pub restart_count: i64,
    pub hostname: String,
    pub environment: Vec<String>,
    pub labels: BTreeMap<String, String>,
    pub tty: bool,
    pub working_dir: String,
    pub restart_policy: String,
    pub memory_limit: i64,
    pub nano_cpus: i64,
    pub mounts: Vec<Mount>,
    pub networks: Vec<NetworkAttachment>,
    pub port_bindings: BTreeMap<String, Vec<String>>,
    /// GPUs the container was given: `--gpus` device requests and GPU device nodes.
    pub gpu_access: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetailsState {
    pub status: ContainerState,
    pub running: bool,
    pub paused: bool,
    pub restarting: bool,
    pub oom_killed: bool,
    pub dead: bool,
    pub pid: i64,
    pub exit_code: i64,
    pub error: String,
    pub started_at: String,
    pub finished_at: String,
    pub health: Option<HealthStatus>,
    pub failing_streak: i64,
}

impl Default for DetailsState {
    fn default() -> Self {
        DetailsState {
            status: ContainerState::Unknown("unknown".into()),
            running: false,
            paused: false,
            restarting: false,
            oom_killed: false,
            dead: false,
            pid: 0,
            exit_code: 0,
            error: String::new(),
            started_at: String::new(),
            finished_at: String::new(),
            health: None,
            failing_streak: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mount {
    pub kind: String,
    pub name: Option<String>,
    pub source: String,
    pub destination: String,
    pub mode: String,
    pub read_write: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetworkAttachment {
    pub name: String,
    pub ip_address: String,
    pub gateway: String,
    pub mac_address: String,
}

impl ContainerDetails {
    pub fn decode(v: &Value) -> Result<Self> {
        let id = require_str(v, "Id", "ContainerDetails")?;
        let s = v.get("State");
        if s.as_object().is_none() {
            return Err(DockerError::Decoding(
                "ContainerDetails: missing 'State'".into(),
            ));
        }
        let health = s.get("Health");
        let (health_status, failing_streak) = if health.as_object().is_some() {
            let raw = health.str_or("Status", "");
            (
                (!raw.is_empty()).then(|| HealthStatus::from_raw(&raw)),
                health.flexible_int("FailingStreak").unwrap_or(0),
            )
        } else {
            (None, 0)
        };
        let state = DetailsState {
            status: s.get("Status").as_str().map_or(
                ContainerState::Unknown("unknown".into()),
                ContainerState::from_raw,
            ),
            running: s.bool_or("Running", false),
            paused: s.bool_or("Paused", false),
            restarting: s.bool_or("Restarting", false),
            oom_killed: s.bool_or("OOMKilled", false),
            dead: s.bool_or("Dead", false),
            pid: s.flexible_int("Pid").unwrap_or(0),
            exit_code: s.flexible_int("ExitCode").unwrap_or(0),
            error: s.str_or("Error", ""),
            started_at: s.str_or("StartedAt", ""),
            finished_at: s.str_or("FinishedAt", ""),
            health: health_status,
            failing_streak,
        };
        let raw_name = v.str_or("Name", "");
        let config = v.get("Config");
        let host_config = v.get("HostConfig");
        let mounts = v
            .get("Mounts")
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|m| Mount {
                        kind: m.str_or("Type", ""),
                        name: m.opt_str("Name"),
                        source: m.str_or("Source", ""),
                        destination: m.str_or("Destination", ""),
                        mode: m.str_or("Mode", ""),
                        read_write: m.bool_or("RW", true),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let settings = v.get("NetworkSettings");
        let mut networks: Vec<NetworkAttachment> = settings
            .get("Networks")
            .as_object()
            .map(|members| {
                members
                    .iter()
                    .filter(|(_, e)| e.as_object().is_some())
                    .map(|(name, e)| NetworkAttachment {
                        name: name.clone(),
                        ip_address: e.str_or("IPAddress", ""),
                        gateway: e.str_or("Gateway", ""),
                        mac_address: e.str_or("MacAddress", ""),
                    })
                    .collect()
            })
            .unwrap_or_default();
        networks.sort_by(|a, b| a.name.cmp(&b.name));
        let mut port_bindings = BTreeMap::new();
        if let Some(members) = settings.get("Ports").as_object() {
            for (port, list) in members {
                let mut bindings = Vec::new();
                for binding in list.as_array().unwrap_or(&[]) {
                    if binding.as_object().is_none() {
                        break;
                    }
                    let host_ip = binding.str_or("HostIp", "");
                    let host_port = binding.str_or("HostPort", "");
                    bindings.push(if host_ip.is_empty() {
                        host_port
                    } else {
                        format!("{host_ip}:{host_port}")
                    });
                }
                port_bindings.insert(port.clone(), bindings);
            }
        }
        Ok(ContainerDetails {
            id,
            name: raw_name.strip_prefix('/').unwrap_or(&raw_name).to_string(),
            created: v.str_or("Created", ""),
            path: v.str_or("Path", ""),
            args: v.string_array("Args"),
            state,
            image: v.str_or("Image", ""),
            image_name: config.str_or("Image", ""),
            restart_count: v.flexible_int("RestartCount").unwrap_or(0),
            hostname: config.str_or("Hostname", ""),
            environment: config.string_array("Env"),
            labels: to_map(config.string_map("Labels")),
            tty: config.bool_or("Tty", false),
            working_dir: config.str_or("WorkingDir", ""),
            restart_policy: host_config.get("RestartPolicy").str_or("Name", ""),
            memory_limit: host_config.flexible_int("Memory").unwrap_or(0),
            nano_cpus: host_config.flexible_int("NanoCpus").unwrap_or(0),
            mounts,
            networks,
            port_bindings,
            gpu_access: gpu_access(host_config),
        })
    }

    /// The command line as Docker would show it.
    pub fn command_line(&self) -> String {
        std::iter::once(&self.path)
            .chain(&self.args)
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn gpu_access(host_config: &Value) -> Vec<String> {
    let mut access = Vec::new();
    for request in host_config.get("DeviceRequests").as_array().unwrap_or(&[]) {
        let capabilities: Option<Vec<Vec<String>>> = request
            .get("Capabilities")
            .as_array()
            .and_then(|a| a.iter().map(Value::as_string_list).collect());
        let is_gpu = capabilities.map_or(true, |caps| {
            caps.iter().any(|set| set.iter().any(|c| c == "gpu"))
        });
        if !is_gpu {
            continue;
        }
        let driver = request
            .get("Driver")
            .as_str()
            .filter(|d| !d.is_empty())
            .unwrap_or("gpu");
        let ids = request
            .get("DeviceIDs")
            .as_string_list()
            .unwrap_or_default();
        if !ids.is_empty() {
            access.push(format!("{driver}: {}", ids.join(", ")));
        } else if let Some(count) = request.flexible_int("Count").filter(|c| *c >= 0) {
            access.push(format!(
                "{driver}: {count} GPU{}",
                if count == 1 { "" } else { "s" }
            ));
        } else {
            access.push(format!("{driver}: all GPUs"));
        }
    }
    let nodes: Vec<String> = host_config
        .get("Devices")
        .as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(|d| d.get("PathOnHost").as_str())
        .filter(|p| p.starts_with("/dev/dri") || *p == "/dev/kfd" || p.starts_with("/dev/nvidia"))
        .map(str::to_string)
        .collect();
    if !nodes.is_empty() {
        access.push(format!("devices: {}", nodes.join(", ")));
    }
    access
}

/// A row from `GET /images/json`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DockerImage {
    pub id: String,
    pub parent_id: String,
    pub repo_tags: Vec<String>,
    pub repo_digests: Vec<String>,
    pub created: i64,
    pub size: i64,
    pub shared_size: i64,
    pub containers: i64,
    pub labels: BTreeMap<String, String>,
}

impl DockerImage {
    pub fn new(id: &str, tags: &[&str], size: i64) -> Self {
        DockerImage {
            id: id.into(),
            parent_id: String::new(),
            repo_tags: tags.iter().map(|t| t.to_string()).collect(),
            repo_digests: Vec::new(),
            created: 0,
            size,
            shared_size: -1,
            containers: -1,
            labels: BTreeMap::new(),
        }
    }

    pub fn decode(v: &Value) -> Result<Self> {
        Ok(DockerImage {
            id: require_str(v, "Id", "DockerImage")?,
            parent_id: v.str_or("ParentId", ""),
            repo_tags: v
                .string_array("RepoTags")
                .into_iter()
                .filter(|t| t != "<none>:<none>")
                .collect(),
            repo_digests: v
                .string_array("RepoDigests")
                .into_iter()
                .filter(|t| t != "<none>@<none>")
                .collect(),
            created: v.flexible_int("Created").unwrap_or(0),
            size: v.flexible_int("Size").unwrap_or(0),
            shared_size: v.flexible_int("SharedSize").unwrap_or(-1),
            containers: v.flexible_int("Containers").unwrap_or(-1),
            labels: to_map(v.string_map("Labels")),
        })
    }

    /// `sha256:abcdef...` -> `abcdef123456`.
    pub fn short_id(&self) -> String {
        self.id
            .strip_prefix("sha256:")
            .unwrap_or(&self.id)
            .chars()
            .take(12)
            .collect()
    }

    pub fn is_dangling(&self) -> bool {
        self.repo_tags.is_empty()
    }

    /// Human-readable reference, falling back to the digest-pinned name or `<none>`.
    pub fn display_name(&self) -> String {
        if let Some(tag) = self.repo_tags.iter().min() {
            return tag.clone();
        }
        if let Some(at) = self
            .repo_digests
            .first()
            .and_then(|d| d.find('@').map(|i| (d, i)))
        {
            return format!("{}@<none>", &at.0[..at.1]);
        }
        "<none>".into()
    }
}

/// A row from `GET /networks`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DockerNetwork {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub created: String,
    pub is_internal: bool,
    pub attachable: bool,
    pub enable_ipv6: bool,
    pub ipam: Vec<IpamConfig>,
    pub container_ids: Vec<String>,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IpamConfig {
    pub subnet: String,
    pub gateway: String,
}

impl DockerNetwork {
    pub fn new(id: &str, name: &str) -> Self {
        DockerNetwork {
            id: id.into(),
            name: name.into(),
            driver: "bridge".into(),
            scope: "local".into(),
            created: String::new(),
            is_internal: false,
            attachable: false,
            enable_ipv6: false,
            ipam: Vec::new(),
            container_ids: Vec::new(),
            labels: BTreeMap::new(),
        }
    }

    pub fn decode(v: &Value) -> Result<Self> {
        let ipam = v
            .get("IPAM")
            .get("Config")
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|c| IpamConfig {
                        subnet: c.str_or("Subnet", ""),
                        gateway: c.str_or("Gateway", ""),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut container_ids: Vec<String> = v
            .get("Containers")
            .as_object()
            .map(|m| m.iter().map(|(k, _)| k.clone()).collect())
            .unwrap_or_default();
        container_ids.sort();
        Ok(DockerNetwork {
            id: require_str(v, "Id", "DockerNetwork")?,
            name: v.str_or("Name", ""),
            driver: v.str_or("Driver", ""),
            scope: v.str_or("Scope", ""),
            created: v.str_or("Created", ""),
            is_internal: v.bool_or("Internal", false),
            attachable: v.bool_or("Attachable", false),
            enable_ipv6: v.bool_or("EnableIPv6", false),
            ipam,
            container_ids,
            labels: to_map(v.string_map("Labels")),
        })
    }

    pub fn short_id(&self) -> String {
        self.id.chars().take(12).collect()
    }

    /// Networks Docker creates itself and refuses to delete.
    pub fn is_built_in(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none" | "nat")
    }

    pub fn compose_project(&self) -> Option<&str> {
        self.labels.get(compose_label::PROJECT).map(String::as_str)
    }
}

/// An entry from `GET /volumes`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DockerVolume {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub created_at: String,
    pub scope: String,
    pub labels: BTreeMap<String, String>,
    pub size_bytes: Option<i64>,
    pub ref_count: Option<i64>,
}

impl DockerVolume {
    pub fn new(name: &str) -> Self {
        DockerVolume {
            name: name.into(),
            driver: "local".into(),
            mountpoint: String::new(),
            created_at: String::new(),
            scope: "local".into(),
            labels: BTreeMap::new(),
            size_bytes: None,
            ref_count: None,
        }
    }

    pub fn decode(v: &Value) -> Result<Self> {
        let usage = v.get("UsageData");
        Ok(DockerVolume {
            name: require_str(v, "Name", "DockerVolume")?,
            driver: v.str_or("Driver", ""),
            mountpoint: v.str_or("Mountpoint", ""),
            created_at: v.str_or("CreatedAt", ""),
            scope: v.str_or("Scope", ""),
            labels: to_map(v.string_map("Labels")),
            // -1 means "not computed" in the API.
            size_bytes: usage.flexible_int("Size").filter(|s| *s >= 0),
            ref_count: usage.flexible_int("RefCount").filter(|s| *s >= 0),
        })
    }

    pub fn id(&self) -> &str {
        &self.name
    }

    pub fn compose_project(&self) -> Option<&str> {
        self.labels.get(compose_label::PROJECT).map(String::as_str)
    }

    /// Anonymous volumes have 64-hex-character generated names.
    pub fn is_anonymous(&self) -> bool {
        self.name.len() == 64 && self.name.chars().all(|c| c.is_ascii_hexdigit())
    }
}

/// Output of `GET /containers/{id}/top`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContainerTop {
    pub titles: Vec<String>,
    pub processes: Vec<Vec<String>>,
}

impl ContainerTop {
    pub fn decode(v: &Value) -> Self {
        ContainerTop {
            titles: v.string_array("Titles"),
            processes: v
                .get("Processes")
                .as_array()
                .and_then(|rows| rows.iter().map(Value::as_string_list).collect())
                .unwrap_or_default(),
        }
    }
}

/// Result of any `POST /{kind}/prune`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PruneReport {
    pub deleted: Vec<String>,
    pub space_reclaimed: i64,
}

impl PruneReport {
    pub fn decode(v: &Value) -> Self {
        let mut deleted = Vec::new();
        for key in ["ContainersDeleted", "VolumesDeleted", "NetworksDeleted"] {
            deleted.extend(v.string_array(key));
        }
        for item in v.get("ImagesDeleted").as_array().unwrap_or(&[]) {
            if item.as_object().is_none() {
                break;
            }
            if let Some(value) = item.opt_str("Deleted").or_else(|| item.opt_str("Untagged")) {
                deleted.push(value);
            }
        }
        PruneReport {
            deleted,
            space_reclaimed: v.flexible_int("SpaceReclaimed").unwrap_or(0),
        }
    }
}

/// `GET /version`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DockerVersion {
    pub version: String,
    pub api_version: String,
    pub min_api_version: String,
    pub os: String,
    pub arch: String,
    pub kernel_version: String,
}

impl DockerVersion {
    pub fn decode(v: &Value) -> Self {
        DockerVersion {
            version: v.str_or("Version", ""),
            api_version: v.str_or("ApiVersion", ""),
            min_api_version: v.str_or("MinAPIVersion", ""),
            os: v.str_or("Os", ""),
            arch: v.str_or("Arch", ""),
            kernel_version: v.str_or("KernelVersion", ""),
        }
    }
}

/// `GET /info`: the host summary for the header.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SystemInfo {
    pub name: String,
    pub server_version: String,
    pub operating_system: String,
    pub os_type: String,
    pub architecture: String,
    pub cpu_count: i64,
    pub memory_total: i64,
    pub containers: i64,
    pub containers_running: i64,
    pub containers_paused: i64,
    pub containers_stopped: i64,
    pub images: i64,
}

impl SystemInfo {
    pub fn decode(v: &Value) -> Self {
        let int = |k: &str| v.flexible_int(k).unwrap_or(0);
        SystemInfo {
            name: v.str_or("Name", ""),
            server_version: v.str_or("ServerVersion", ""),
            operating_system: v.str_or("OperatingSystem", ""),
            os_type: v.str_or("OSType", ""),
            architecture: v.str_or("Architecture", ""),
            cpu_count: int("NCPU"),
            memory_total: int("MemTotal"),
            containers: int("Containers"),
            containers_running: int("ContainersRunning"),
            containers_paused: int("ContainersPaused"),
            containers_stopped: int("ContainersStopped"),
            images: int("Images"),
        }
    }
}

/// One message from `GET /events`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DockerEvent {
    pub kind: String,
    pub action: String,
    pub actor_id: String,
    pub attributes: BTreeMap<String, String>,
    pub time: i64,
    pub time_nano: i64,
}

impl DockerEvent {
    pub fn new(kind: &str, action: &str, actor_id: &str) -> Self {
        DockerEvent {
            kind: kind.into(),
            action: action.into(),
            actor_id: actor_id.into(),
            ..Default::default()
        }
    }

    pub fn decode(v: &Value) -> Self {
        let action = v.str_or("Action", "");
        let actor = v.get("Actor");
        let (actor_id, attributes) = if actor.as_object().is_some() {
            (
                actor.str_or("ID", ""),
                to_map(actor.string_map("Attributes")),
            )
        } else {
            (v.str_or("id", ""), BTreeMap::new())
        };
        DockerEvent {
            kind: v.str_or("Type", ""),
            action: if action.is_empty() {
                v.str_or("status", "")
            } else {
                action
            },
            actor_id,
            attributes,
            time: v.flexible_int("time").unwrap_or(0),
            time_nano: v.flexible_int("timeNano").unwrap_or(0),
        }
    }

    /// Exec events look like `exec_start: sh -c ...`; this strips the payload to get the verb.
    pub fn base_action(&self) -> String {
        self.action
            .split(':')
            .next()
            .unwrap_or(&self.action)
            .trim()
            .to_string()
    }

    /// Events that change what a container or resource list should show.
    pub fn affects_inventory(&self) -> bool {
        match self.kind.as_str() {
            "container" => matches!(
                self.base_action().as_str(),
                "create"
                    | "start"
                    | "stop"
                    | "die"
                    | "kill"
                    | "pause"
                    | "unpause"
                    | "restart"
                    | "destroy"
                    | "rename"
                    | "update"
                    | "health_status"
                    | "oom"
            ),
            "image" | "network" | "volume" => true,
            _ => false,
        }
    }
}

/// Decodes a JSON array of models, failing if any element lacks its identity.
pub fn decode_list<T>(v: &Value, decode: impl Fn(&Value) -> Result<T>) -> Result<Vec<T>> {
    v.as_array()
        .ok_or_else(|| DockerError::Decoding("expected an array".into()))?
        .iter()
        .map(decode)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse_str;

    #[test]
    fn decodes_container_with_nulls_and_string_numbers() {
        let v = parse_str(
            r#"{"Id":"abcdef0123456789","Names":["/web","/alias/web"],"Image":"nginx","Created":"1700000000",
                "Ports":[{"PrivatePort":80,"PublicPort":8080,"Type":"tcp","IP":"0.0.0.0"},{"PrivatePort":443}],
                "Labels":null,"State":"running","Status":"Up 3 minutes (healthy)"}"#,
        )
        .unwrap();
        let c = Container::decode(&v).unwrap();
        assert_eq!(c.name(), "web");
        assert_eq!(c.short_id(), "abcdef012345");
        assert_eq!(c.created, 1_700_000_000);
        assert_eq!(c.health(), Some(HealthStatus::Healthy));
        assert_eq!(c.port_summary(), "0.0.0.0:8080->80/tcp, 443/tcp");
        assert!(c.labels.is_empty());
    }

    #[test]
    fn container_requires_id() {
        assert!(Container::decode(&parse_str(r#"{"Names":[]}"#).unwrap()).is_err());
    }

    #[test]
    fn memory_excludes_cache_for_cgroup_v1_and_v2() {
        let v2 = ContainerStats::decode(
            &parse_str(r#"{"memory_stats":{"usage":1000,"stats":{"inactive_file":200}}}"#).unwrap(),
        );
        assert_eq!(v2.memory_usage_excluding_cache(), 800);
        let v1 = ContainerStats::decode(
            &parse_str(r#"{"memory_stats":{"usage":1000,"stats":{"total_inactive_file":300,"inactive_file":100}}}"#)
                .unwrap(),
        );
        assert_eq!(v1.memory_usage_excluding_cache(), 700);
        let windows = ContainerStats::decode(
            &parse_str(r#"{"memory_stats":{"privateworkingset":42}}"#).unwrap(),
        );
        assert_eq!(windows.memory_usage_excluding_cache(), 42);
    }

    #[test]
    fn block_io_accepts_both_cgroup_spellings() {
        let s = ContainerStats::decode(
            &parse_str(
                r#"{"blkio_stats":{"io_service_bytes_recursive":[{"op":"Read","value":5},{"op":"write","value":7},{"op":"Total","value":12}]}}"#,
            )
            .unwrap(),
        );
        assert_eq!(s.total_block_io(), (5, 7));
    }

    #[test]
    fn details_collect_gpu_access() {
        let v = parse_str(
            r#"{"Id":"x","State":{"Status":"running","Health":{"Status":"unhealthy","FailingStreak":3}},
                "HostConfig":{"DeviceRequests":[{"Driver":"nvidia","Count":-1,"Capabilities":[["gpu"]]},{"Count":1}],
                "Devices":[{"PathOnHost":"/dev/dri/renderD128"},{"PathOnHost":"/dev/null"}]},
                "Path":"sh","Args":["-c","sleep 1"]}"#,
        )
        .unwrap();
        let d = ContainerDetails::decode(&v).unwrap();
        assert_eq!(
            d.gpu_access,
            vec![
                "nvidia: all GPUs",
                "gpu: 1 GPU",
                "devices: /dev/dri/renderD128"
            ]
        );
        assert_eq!(d.state.health, Some(HealthStatus::Unhealthy));
        assert_eq!(d.state.failing_streak, 3);
        assert_eq!(d.command_line(), "sh -c sleep 1");
    }

    #[test]
    fn image_display_name_falls_back() {
        let mut image = DockerImage::new("sha256:0123456789abcdef", &[], 0);
        assert_eq!(image.display_name(), "<none>");
        image.repo_digests = vec!["repo@sha256:x".into()];
        assert_eq!(image.display_name(), "repo@<none>");
        assert_eq!(image.short_id(), "0123456789ab");
    }

    #[test]
    fn events_support_legacy_fields() {
        let e = DockerEvent::decode(
            &parse_str(r#"{"status":"exec_start: sh","id":"abc","Type":"container"}"#).unwrap(),
        );
        assert_eq!(e.base_action(), "exec_start");
        assert_eq!(e.actor_id, "abc");
        assert!(!e.affects_inventory());
    }

    #[test]
    fn prune_report_collects_every_kind() {
        let r = PruneReport::decode(
            &parse_str(r#"{"ImagesDeleted":[{"Untagged":"a"},{"Deleted":"b"}],"VolumesDeleted":["v"],"SpaceReclaimed":10}"#)
                .unwrap(),
        );
        assert_eq!(r.deleted, vec!["v", "a", "b"]);
        assert_eq!(r.space_reclaimed, 10);
    }
}
