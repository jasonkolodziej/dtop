//! GPU monitoring (ADR 0002): NVIDIA via `nvidia-smi` (Linux, Windows), AMD and Intel via
//! sysfs and DRM fdinfo (Linux), Apple via IOKit (macOS). Per-process usage is attributed to
//! containers through `/proc/<pid>/cgroup`, which only works on the Docker host itself.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::compose::{resolve_executable, CommandInvocation, CommandRunning, ProcessCommandRunner};
use crate::error::DockerError;
use crate::platform::Platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Unknown,
}

impl GpuVendor {
    pub fn display_name(self) -> &'static str {
        match self {
            GpuVendor::Nvidia => "NVIDIA",
            GpuVendor::Amd => "AMD",
            GpuVendor::Intel => "Intel",
            GpuVendor::Apple => "Apple",
            GpuVendor::Unknown => "GPU",
        }
    }

    /// PCI vendor IDs as found in `/sys/class/drm/cardN/device/vendor`.
    pub fn from_pci_vendor_id(id: &str) -> Self {
        match id.trim().to_lowercase().as_str() {
            "0x10de" => GpuVendor::Nvidia,
            "0x1002" => GpuVendor::Amd,
            "0x8086" => GpuVendor::Intel,
            _ => GpuVendor::Unknown,
        }
    }
}

/// One GPU and its readings; each vendor exposes a different subset.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuDevice {
    pub id: String,
    pub vendor: GpuVendor,
    pub name: String,
    /// Busy percentage, 0-100.
    pub utilization: Option<f64>,
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    pub temperature: Option<f64>,
}

impl GpuDevice {
    pub fn new(id: &str, vendor: GpuVendor, name: &str) -> Self {
        GpuDevice {
            id: id.into(),
            vendor,
            name: name.into(),
            utilization: None,
            memory_used: None,
            memory_total: None,
            temperature: None,
        }
    }
}

/// GPU usage attributed to one host process.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuProcessUsage {
    pub pid: i64,
    pub device_id: String,
    /// Share of the device this process kept busy, 0-100.
    pub utilization: Option<f64>,
    pub memory_bytes: Option<u64>,
    /// The Docker container the process belongs to (from its cgroup).
    pub container_id: Option<String>,
}

/// One GPU polling round.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GpuSample {
    pub devices: Vec<GpuDevice>,
    pub processes: Vec<GpuProcessUsage>,
    /// True when every GPU process could be inspected, so containers without entries really use
    /// no GPU (rather than "unknown").
    pub process_attribution_complete: bool,
    /// Human-readable limitations, e.g. missing permissions for per-process data.
    pub issues: Vec<String>,
}

impl GpuSample {
    /// Mean utilization across devices that report one.
    pub fn average_utilization(&self) -> Option<f64> {
        let values: Vec<f64> = self.devices.iter().filter_map(|d| d.utilization).collect();
        (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
    }

    pub fn total_memory_used(&self) -> Option<u64> {
        let values: Vec<u64> = self.devices.iter().filter_map(|d| d.memory_used).collect();
        (!values.is_empty()).then(|| values.iter().fold(0u64, |a, b| a.wrapping_add(*b)))
    }

    pub fn total_memory(&self) -> Option<u64> {
        let values: Vec<u64> = self.devices.iter().filter_map(|d| d.memory_total).collect();
        (!values.is_empty()).then(|| values.iter().fold(0u64, |a, b| a.wrapping_add(*b)))
    }

    /// Usage per container: utilization summed per device then averaged across all devices (one
    /// container saturating one of two GPUs reads 50%), memory summed.
    pub fn usage_by_container(&self) -> BTreeMap<String, (f64, u64)> {
        let device_count = self.devices.len().max(1) as f64;
        let mut result: BTreeMap<String, (f64, u64)> = BTreeMap::new();
        for p in &self.processes {
            let Some(container) = &p.container_id else {
                continue;
            };
            let entry = result.entry(container.clone()).or_insert((0.0, 0));
            entry.0 += p.utilization.unwrap_or(0.0) / device_count;
            entry.1 = entry.1.wrapping_add(p.memory_bytes.unwrap_or(0));
        }
        for value in result.values_mut() {
            value.0 = value.0.min(100.0);
        }
        result
    }
}

/// Reads GPU state. Implementations keep state between calls to turn counters into rates, so
/// keep one instance for the life of the app.
pub trait GpuCollecting: Send + Sync {
    fn sample(&self, now: f64) -> GpuSample;
}

/// The 64-hex container ID in `/proc/<pid>/cgroup`: cgroup v1 (`/docker/<id>`), v2 with
/// systemd (`docker-<id>.scope`) and other runtimes.
pub fn cgroup_container_id(cgroup_file: &str) -> Option<String> {
    cgroup_file
        .lines()
        .find_map(|line| line.splitn(3, ':').last().and_then(first_container_id))
}

fn first_container_id(path: &str) -> Option<String> {
    let mut run = String::new();
    let mut best = None;
    for c in path.chars() {
        if c.is_ascii_hexdigit() && !c.is_ascii_uppercase() {
            run.push(c);
            continue;
        }
        if run.len() == 64 {
            best = Some(run.clone());
        }
        run.clear();
    }
    if run.len() == 64 {
        best = Some(run);
    }
    best
}

/// `nvidia-smi` invocations and parsers (pure, tested with recorded output).
pub mod nvidia_smi {
    use super::*;

    pub const DEVICE_QUERY_ARGUMENTS: [&str; 2] = [
        "--query-gpu=index,uuid,name,utilization.gpu,memory.used,memory.total,temperature.gpu",
        "--format=csv,noheader,nounits",
    ];
    pub const COMPUTE_APPS_ARGUMENTS: [&str; 2] = [
        "--query-compute-apps=pid,gpu_uuid,used_memory",
        "--format=csv,noheader,nounits",
    ];
    /// One sample of per-process utilization (`sm` column).
    /// `u`: SM utilization; `m`: framebuffer memory (`fb`, MiB), which also covers graphics
    /// processes that `--query-compute-apps` leaves out.
    pub const PROCESS_MONITOR_ARGUMENTS: [&str; 5] = ["pmon", "-c", "1", "-s", "um"];

    /// One `pmon` row.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ProcessSample {
        pub gpu: i64,
        pub pid: i64,
        /// SM utilization, %.
        pub sm: Option<f64>,
        /// Framebuffer memory, bytes.
        pub memory: Option<u64>,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct Device {
        pub index: i64,
        pub uuid: String,
        pub device: GpuDevice,
    }

    fn split_csv(line: &str) -> Vec<String> {
        line.split(',').map(|f| f.trim().to_string()).collect()
    }

    /// Numbers, or None for `[N/A]`, `[Not Supported]`, `N/A`, `-` and empty fields.
    pub fn number(text: &str) -> Option<f64> {
        let value = text.trim();
        if value.is_empty() || value.starts_with('[') || value == "N/A" || value == "-" {
            return None;
        }
        value.parse().ok()
    }

    const MIB: f64 = 1_048_576.0;

    /// `0, GPU-8f6c…, NVIDIA GeForce RTX 3080, 45, 3210, 10240, 61` (memory in MiB).
    pub fn parse_devices(output: &str) -> Vec<Device> {
        output
            .lines()
            .filter_map(|line| {
                let f = split_csv(line);
                if f.len() < 6 {
                    return None;
                }
                let index: i64 = f[0].parse().ok()?;
                let mut device =
                    GpuDevice::new(&format!("nvidia:{}", f[1]), GpuVendor::Nvidia, &f[2]);
                device.utilization = number(&f[3]);
                device.memory_used = number(&f[4]).map(|m| (m * MIB) as u64);
                device.memory_total = number(&f[5]).map(|m| (m * MIB) as u64);
                device.temperature = f.get(6).and_then(|t| number(t));
                Some(Device {
                    index,
                    uuid: f[1].clone(),
                    device,
                })
            })
            .collect()
    }

    /// `4242, GPU-8f6c…, 1536` → pid, device UUID, used memory.
    pub fn parse_compute_apps(output: &str) -> Vec<(i64, String, Option<u64>)> {
        output
            .lines()
            .filter_map(|line| {
                let f = split_csv(line);
                if f.len() < 3 {
                    return None;
                }
                Some((
                    f[0].parse().ok()?,
                    f[1].clone(),
                    number(&f[2]).map(|m| (m * MIB) as u64),
                ))
            })
            .collect()
    }

    /// `nvidia-smi pmon -c 1 -s um` rows. Columns are found by the header's names (drivers add
    /// columns such as `jpg`, `ofa` and `ccpm`); `-` means no sample.
    pub fn parse_process_monitor(output: &str) -> Vec<ProcessSample> {
        let mut columns: Vec<String> = Vec::new();
        let mut rows = Vec::new();
        for raw in output.lines() {
            let line = raw.trim();
            if let Some(header) = line.strip_prefix('#') {
                if columns.is_empty() {
                    columns = header.split_whitespace().map(str::to_lowercase).collect();
                }
                continue;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            let position = |name: &str, default: Option<usize>| {
                columns.iter().position(|c| c == name).or(default)
            };
            let (Some(gpu), Some(pid)) = (position("gpu", Some(0)), position("pid", Some(1)))
            else {
                continue;
            };
            let field = |index: Option<usize>| index.and_then(|i| fields.get(i).copied());
            let (Some(Ok(index)), Some(Ok(p))) = (
                field(Some(gpu)).map(str::parse::<i64>),
                field(Some(pid)).map(str::parse::<i64>),
            ) else {
                continue;
            };
            rows.push(ProcessSample {
                gpu: index,
                pid: p,
                sm: field(position("sm", Some(3))).and_then(number),
                memory: field(position("fb", None))
                    .and_then(number)
                    .map(|mib| (mib * MIB) as u64),
            });
        }
        rows
    }
}

/// One DRM client from `/proc/<pid>/fdinfo/<fd>` (kernel "DRM client usage stats"). amdgpu
/// and i915 report `drm-engine-<name>` busy nanoseconds; xe reports cycles.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DrmClient {
    pub driver: String,
    pub client_id: String,
    pub pdev: String,
    pub engine_nanoseconds: BTreeMap<String, Option<u64>>,
    pub engine_capacity: BTreeMap<String, Option<u64>>,
    pub cycles: BTreeMap<String, Option<u64>>,
    pub total_cycles: BTreeMap<String, Option<u64>>,
    /// Bytes resident per memory region (`drm-resident-*`, or amdgpu's legacy `drm-memory-*`).
    pub resident_memory: BTreeMap<String, Option<u64>>,
}

impl DrmClient {
    /// Clients are unique per device, and several fds of one client must not be double counted.
    pub fn key(&self) -> String {
        format!("{}#{}", self.pdev, self.client_id)
    }

    /// Memory in device-local regions (VRAM / Intel "local").
    pub fn device_memory(&self) -> u64 {
        self.resident_memory
            .iter()
            .filter(|(region, _)| region.starts_with("vram") || region.starts_with("local"))
            .filter_map(|(_, v)| *v)
            .fold(0u64, u64::wrapping_add)
    }

    pub fn parse(text: &str) -> Option<DrmClient> {
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            if key.starts_with("drm-") {
                fields.insert(key.to_string(), value.trim().to_string());
            }
        }
        let mut client = DrmClient {
            driver: fields.get("drm-driver")?.clone(),
            client_id: fields.get("drm-client-id")?.clone(),
            pdev: fields.get("drm-pdev").cloned().unwrap_or_default(),
            ..Default::default()
        };
        let mut legacy: BTreeMap<String, Option<u64>> = BTreeMap::new();
        for (key, value) in &fields {
            if let Some(engine) = key.strip_prefix("drm-engine-capacity-") {
                client
                    .engine_capacity
                    .insert(engine.into(), value.parse().ok());
            } else if let Some(engine) = key.strip_prefix("drm-engine-") {
                client.engine_nanoseconds.insert(
                    engine.into(),
                    value.split(' ').next().and_then(|n| n.parse().ok()),
                );
            } else if let Some(engine) = key.strip_prefix("drm-total-cycles-") {
                client
                    .total_cycles
                    .insert(engine.into(), value.parse().ok());
            } else if let Some(engine) = key.strip_prefix("drm-cycles-") {
                client.cycles.insert(engine.into(), value.parse().ok());
            } else if let Some(region) = key.strip_prefix("drm-resident-") {
                client
                    .resident_memory
                    .insert(region.into(), parse_bytes(value));
            } else if let Some(region) = key.strip_prefix("drm-memory-") {
                legacy.insert(region.into(), parse_bytes(value));
            }
        }
        // drm-memory-<region> is a deprecated amdgpu alias for drm-resident-<region>.
        for (region, bytes) in legacy {
            client.resident_memory.entry(region).or_insert(bytes);
        }
        Some(client)
    }
}

/// `23992 KiB`, `16 MiB` or plain bytes.
pub fn parse_bytes(value: &str) -> Option<u64> {
    let mut parts = value.split(' ');
    let number: u64 = parts.next()?.parse().ok()?;
    Some(match parts.next().unwrap_or("") {
        "KiB" => number * 1024,
        "MiB" => number * 1_048_576,
        "GiB" => number * 1_073_741_824,
        _ => number,
    })
}

/// Per-engine utilization (0-100) of one client over the last interval.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DrmClientUsage {
    pub engines: BTreeMap<String, f64>,
}

impl DrmClientUsage {
    /// The busiest engine: what "GPU %" means for one process.
    pub fn busiest(&self) -> f64 {
        self.engines.values().copied().fold(0.0, f64::max)
    }
}

#[derive(Debug, Clone)]
struct Previous {
    timestamp: f64,
    engine_nanoseconds: BTreeMap<String, u64>,
    cycles: BTreeMap<String, u64>,
    total_cycles: BTreeMap<String, u64>,
}

fn present(map: &BTreeMap<String, Option<u64>>) -> BTreeMap<String, u64> {
    map.iter()
        .filter_map(|(k, v)| v.map(|v| (k.clone(), v)))
        .collect()
}

/// Turns cumulative DRM counters into utilization between two polls.
#[derive(Debug, Clone, Default)]
pub struct DrmUsageTracker {
    previous: HashMap<String, Previous>,
}

impl DrmUsageTracker {
    /// Usage per client key since the previous call. New clients have no baseline and are
    /// absent. A counter that goes backwards keeps its previous larger value (per the spec).
    pub fn update(&mut self, clients: &[DrmClient], now: f64) -> HashMap<String, DrmClientUsage> {
        let mut result = HashMap::new();
        let mut next: HashMap<String, Previous> = HashMap::new();
        for client in clients {
            let key = client.key();
            if next.contains_key(&key) {
                continue;
            }
            let old = self.previous.get(&key);
            let mut current = Previous {
                timestamp: now,
                engine_nanoseconds: present(&client.engine_nanoseconds),
                cycles: present(&client.cycles),
                total_cycles: present(&client.total_cycles),
            };
            if let Some(old) = old {
                for (engine, value) in current.engine_nanoseconds.iter_mut() {
                    if let Some(&o) = old.engine_nanoseconds.get(engine).filter(|o| *value < **o) {
                        *value = o;
                    }
                }
                for (engine, value) in current.cycles.iter_mut() {
                    if let Some(&o) = old.cycles.get(engine).filter(|o| *value < **o) {
                        *value = o;
                    }
                }
            }
            next.insert(key.clone(), current.clone());
            let Some(old) = old else { continue };
            let capacity = |engine: &str| {
                client
                    .engine_capacity
                    .get(engine)
                    .copied()
                    .flatten()
                    .unwrap_or(1)
                    .max(1) as f64
            };
            let mut engines: BTreeMap<String, f64> = BTreeMap::new();
            let elapsed = (now - old.timestamp) * 1_000_000_000.0;
            if elapsed > 0.0 {
                for (engine, &value) in &current.engine_nanoseconds {
                    let baseline = old.engine_nanoseconds.get(engine).copied().unwrap_or(value);
                    let delta = (value - value.min(baseline)) as f64;
                    engines.insert(engine.clone(), delta / (elapsed * capacity(engine)) * 100.0);
                }
            }
            for (engine, &value) in &current.cycles {
                let (Some(&total), Some(&old_total)) = (
                    current.total_cycles.get(engine),
                    old.total_cycles.get(engine),
                ) else {
                    continue;
                };
                if total <= old_total {
                    continue;
                }
                let baseline = old.cycles.get(engine).copied().unwrap_or(value);
                let delta = (value - value.min(baseline)) as f64;
                let share = delta / ((total - old_total) as f64 * capacity(engine)) * 100.0;
                let entry = engines.entry(engine.clone()).or_insert(0.0);
                *entry = entry.max(share);
            }
            for v in engines.values_mut() {
                *v = v.clamp(0.0, 100.0);
            }
            result.insert(key, DrmClientUsage { engines });
        }
        self.previous = next;
        result
    }

    /// Device utilization: per engine sum every client's share (capped at 100%), then take the
    /// busiest engine.
    pub fn device_utilization(usages: &[DrmClientUsage]) -> Option<f64> {
        if usages.is_empty() {
            return None;
        }
        let mut per_engine: BTreeMap<&str, f64> = BTreeMap::new();
        for usage in usages {
            for (engine, value) in &usage.engines {
                *per_engine.entry(engine).or_insert(0.0) += value;
            }
        }
        Some(per_engine.values().copied().fold(0.0, f64::max).min(100.0))
    }
}

/// The result of a lookup that tells "not there" from "not allowed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access<T> {
    Found(T),
    /// Gone or never there, e.g. a process or fd that ended during the scan.
    Missing,
    /// The kernel refused (another user's process without root or `CAP_SYS_PTRACE`).
    Denied,
}

impl<T> Access<T> {
    fn from_io(result: std::io::Result<T>) -> Self {
        match result {
            Ok(value) => Access::Found(value),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Access::Denied,
            Err(_) => Access::Missing,
        }
    }

    fn from_option(value: Option<T>) -> Self {
        value.map_or(Access::Missing, Access::Found)
    }
}

/// File access the collector needs, injectable for tests against a fake `/sys` + `/proc`.
pub trait GpuFileSystem: Send + Sync {
    fn read(&self, path: &str) -> Option<String>;
    fn list(&self, path: &str) -> Option<Vec<String>>;
    fn read_link(&self, path: &str) -> Option<String>;

    /// `list`, `read_link` and `read` that report permission denials, which make DRM totals
    /// incomplete; plain failures (the process exited meanwhile) do not.
    fn list_access(&self, path: &str) -> Access<Vec<String>> {
        Access::from_option(self.list(path))
    }
    fn read_link_access(&self, path: &str) -> Access<String> {
        Access::from_option(self.read_link(path))
    }
    fn read_access(&self, path: &str) -> Access<String> {
        Access::from_option(self.read(path))
    }
}

pub struct LocalGpuFileSystem;

impl GpuFileSystem for LocalGpuFileSystem {
    fn read(&self, path: &str) -> Option<String> {
        // procfs/sysfs report a size of 0, so read to EOF instead of trusting stat().
        let bytes = std::fs::read(path).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn list(&self, path: &str) -> Option<Vec<String>> {
        let entries = std::fs::read_dir(path).ok()?;
        Some(
            entries
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .collect(),
        )
    }

    fn read_link(&self, path: &str) -> Option<String> {
        std::fs::read_link(path)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }

    fn list_access(&self, path: &str) -> Access<Vec<String>> {
        Access::from_io(std::fs::read_dir(path).map(|entries| {
            entries
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .collect()
        }))
    }

    fn read_link_access(&self, path: &str) -> Access<String> {
        Access::from_io(std::fs::read_link(path).map(|p| p.to_string_lossy().into_owned()))
    }

    fn read_access(&self, path: &str) -> Access<String> {
        Access::from_io(std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()))
    }
}

/// How long one `nvidia-smi` query may take. It normally answers in well under a second (a
/// first call without persistence mode can take a couple of seconds); a hung driver can make it
/// block indefinitely, and it runs inside the host refresh.
pub const NVIDIA_SMI_TIMEOUT: Duration = Duration::from_secs(5);
/// After a timed-out query, how long to leave `nvidia-smi` alone before trying again, so a hung
/// driver costs one timeout per period instead of one per refresh.
pub const NVIDIA_SMI_RETRY_SECONDS: f64 = 30.0;

/// The outcome of one `nvidia-smi` query.
enum SmiOutput {
    Output(String),
    Failed,
    TimedOut,
}

/// GPUs on Linux (NVIDIA via `nvidia-smi`, AMD via amdgpu sysfs, AMD/Intel per process via DRM
/// fdinfo) and Windows (NVIDIA via `nvidia-smi`).
pub struct SystemGpuCollector {
    pub platform: Platform,
    pub nvidia_smi: Option<String>,
    file_system: Arc<dyn GpuFileSystem>,
    runner: Arc<dyn CommandRunning>,
    proc_root: String,
    drm_root: String,
    /// amdkfd's sysfs (`/sys/class/kfd/kfd`): per-process data for ROCm compute work, which
    /// bypasses the DRM scheduler and so never shows in fdinfo engine time.
    kfd_root: String,
    tracker: Mutex<DrmUsageTracker>,
    /// Bound on each `nvidia-smi` query (`NVIDIA_SMI_TIMEOUT` unless a test shortens it).
    pub nvidia_smi_timeout: Duration,
    /// When `nvidia-smi` may be tried again after a timeout (seconds, the `sample` clock).
    nvidia_smi_retry_at: Mutex<Option<f64>>,
    /// The NVIDIA GPUs from the last answer, shown without readings while `nvidia-smi` is not
    /// answering, so the GPU box stays up and explains why.
    nvidia_last_devices: Mutex<Vec<GpuDevice>>,
}

impl SystemGpuCollector {
    pub fn new(
        platform: Platform,
        nvidia_smi: Option<String>,
        file_system: Arc<dyn GpuFileSystem>,
        runner: Arc<dyn CommandRunning>,
        proc_root: &str,
        drm_root: &str,
    ) -> Self {
        SystemGpuCollector {
            platform,
            nvidia_smi,
            file_system,
            runner,
            proc_root: proc_root.into(),
            drm_root: drm_root.into(),
            kfd_root: "/sys/class/kfd/kfd".into(),
            tracker: Mutex::new(DrmUsageTracker::default()),
            nvidia_smi_timeout: NVIDIA_SMI_TIMEOUT,
            nvidia_smi_retry_at: Mutex::new(None),
            nvidia_last_devices: Mutex::new(Vec::new()),
        }
    }

    /// The collector for this machine, or None when the platform has no supported source.
    pub fn make_default(path: &str, platform: Platform) -> Option<Arc<dyn GpuCollecting>> {
        match platform {
            Platform::MacOS => {
                #[cfg(target_os = "macos")]
                {
                    Some(Arc::new(apple::AppleGpuCollector))
                }
                #[cfg(not(target_os = "macos"))]
                {
                    None
                }
            }
            Platform::Linux | Platform::Windows => {
                let smi = resolve_executable("nvidia-smi", path, platform, &|p| {
                    std::path::Path::new(p).is_file()
                });
                Some(Arc::new(SystemGpuCollector::new(
                    platform,
                    smi,
                    Arc::new(LocalGpuFileSystem),
                    Arc::new(ProcessCommandRunner::default()),
                    "/proc",
                    "/sys/class/drm",
                )))
            }
            Platform::Other => None,
        }
    }

    /// Runs one `nvidia-smi` query. Failures of `required` queries are reported as issues;
    /// timeouts always are.
    fn run(
        &self,
        executable: &str,
        arguments: &[&str],
        required: bool,
        result: &mut GpuSample,
    ) -> SmiOutput {
        let invocation = CommandInvocation {
            executable: executable.into(),
            arguments: arguments.iter().map(|a| a.to_string()).collect(),
            working_directory: None,
            environment_remove: Vec::new(),
        };
        match self
            .runner
            .run_with_timeout(&invocation, self.nvidia_smi_timeout)
        {
            Ok(output) if output.succeeded() => SmiOutput::Output(output.output),
            Ok(output) => {
                if required {
                    let reason = output
                        .output
                        .lines()
                        .next()
                        .map_or_else(|| format!("exit code {}", output.exit_code), str::to_string);
                    result.issues.push(format!("nvidia-smi failed: {reason}"));
                }
                SmiOutput::Failed
            }
            Err(DockerError::TimedOut(_)) => SmiOutput::TimedOut,
            Err(error) => {
                if required {
                    let reason = match error {
                        DockerError::Io(reason) => reason,
                        other => other.to_string(),
                    };
                    result
                        .issues
                        .push(format!("nvidia-smi could not run: {reason}"));
                }
                SmiOutput::Failed
            }
        }
    }

    /// The last known NVIDIA GPUs with their readings cleared ("n/a", not stale numbers).
    fn unanswered_nvidia_devices(&self, result: &mut GpuSample) {
        // Whatever ran on the NVIDIA GPUs this time is unknown, so containers without other
        // GPU entries are unknown too, not idle (also on hosts with AMD/Intel GPUs as well).
        result.process_attribution_complete = false;
        let known = self
            .nvidia_last_devices
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default();
        if known.is_empty() || result.devices.iter().any(|d| d.vendor == GpuVendor::Nvidia) {
            return;
        }
        result.devices.extend(known.into_iter().map(|d| GpuDevice {
            utilization: None,
            memory_used: None,
            temperature: None,
            ..d
        }));
    }

    fn nvidia_smi_hung_issue(&self, retry_in: f64) -> String {
        format!(
            "nvidia-smi did not answer within {} s and was stopped — the NVIDIA driver may be \
             hung; retrying in {:.0} s",
            self.nvidia_smi_timeout.as_secs_f64(),
            retry_in.ceil()
        )
    }

    /// Records a timed-out query: no more `nvidia-smi` until the retry time.
    fn nvidia_smi_timed_out(&self, now: f64, result: &mut GpuSample) {
        if let Ok(mut retry_at) = self.nvidia_smi_retry_at.lock() {
            *retry_at = Some(now + NVIDIA_SMI_RETRY_SECONDS);
        }
        self.unanswered_nvidia_devices(result);
        result
            .issues
            .push(self.nvidia_smi_hung_issue(NVIDIA_SMI_RETRY_SECONDS));
    }

    fn collect_nvidia(&self, executable: &str, now: f64, result: &mut GpuSample) {
        let waiting = self
            .nvidia_smi_retry_at
            .lock()
            .ok()
            .and_then(|r| *r)
            .filter(|at| *at > now);
        if let Some(at) = waiting {
            result.issues.push(self.nvidia_smi_hung_issue(at - now));
            return self.unanswered_nvidia_devices(result);
        }
        // A killed nvidia-smi still stuck in the driver: starting another would only pile up
        // stuck processes.
        if self.runner.abandoned_processes() > 0 {
            result.issues.push(
                "nvidia-smi is still stuck in the NVIDIA driver after being stopped; waiting \
                 for it to exit"
                    .into(),
            );
            return self.unanswered_nvidia_devices(result);
        }
        let output = match self.run(
            executable,
            &nvidia_smi::DEVICE_QUERY_ARGUMENTS,
            true,
            result,
        ) {
            SmiOutput::Output(output) => output,
            SmiOutput::Failed => return,
            SmiOutput::TimedOut => return self.nvidia_smi_timed_out(now, result),
        };
        let devices = nvidia_smi::parse_devices(&output);
        if devices.is_empty() {
            return;
        }
        if let Ok(mut known) = self.nvidia_last_devices.lock() {
            *known = devices.iter().map(|d| d.device.clone()).collect();
        }
        result
            .devices
            .extend(devices.iter().map(|d| d.device.clone()));
        let apps = match self.run(
            executable,
            &nvidia_smi::COMPUTE_APPS_ARGUMENTS,
            true,
            result,
        ) {
            SmiOutput::Output(o) => nvidia_smi::parse_compute_apps(&o),
            SmiOutput::Failed => Vec::new(),
            SmiOutput::TimedOut => {
                // The devices are known; per-process data waits for the retry.
                result.process_attribution_complete = false;
                return self.nvidia_smi_timed_out(now, result);
            }
        };
        // pmon is optional (unsupported on some GPUs); without it only per-process % is lost.
        let monitor = match self.run(
            executable,
            &nvidia_smi::PROCESS_MONITOR_ARGUMENTS,
            false,
            result,
        ) {
            SmiOutput::Output(o) => nvidia_smi::parse_process_monitor(&o),
            SmiOutput::Failed => Vec::new(),
            SmiOutput::TimedOut => {
                self.nvidia_smi_timed_out(now, result);
                Vec::new()
            }
        };
        let mut uuid_by_index: HashMap<i64, String> = HashMap::new();
        for d in &devices {
            uuid_by_index
                .entry(d.index)
                .or_insert_with(|| d.uuid.clone());
        }
        let mut usage: BTreeMap<String, GpuProcessUsage> = BTreeMap::new();
        for (pid, uuid, memory) in apps {
            usage.insert(
                format!("{uuid}#{pid}"),
                GpuProcessUsage {
                    pid,
                    device_id: format!("nvidia:{uuid}"),
                    utilization: None,
                    memory_bytes: memory,
                    container_id: None,
                },
            );
        }
        for row in monitor {
            let Some(uuid) = uuid_by_index.get(&row.gpu) else {
                continue;
            };
            let entry = usage
                .entry(format!("{uuid}#{}", row.pid))
                .or_insert_with(|| GpuProcessUsage {
                    pid: row.pid,
                    device_id: format!("nvidia:{uuid}"),
                    utilization: None,
                    memory_bytes: None,
                    container_id: None,
                });
            entry.utilization = row.sm;
            // --query-compute-apps is exact for compute processes; pmon's `fb` fills in the
            // graphics (and compute+graphics) ones it does not list.
            if entry.memory_bytes.is_none() {
                entry.memory_bytes = row.memory;
            }
        }
        let mut processes: Vec<GpuProcessUsage> = usage.into_values().collect();
        processes.sort_by_key(|p| p.pid);
        for mut process in processes {
            if self.platform == Platform::Linux {
                process.container_id = self.container_id(process.pid);
            }
            result.processes.push(process);
        }
        if self.platform != Platform::Linux {
            result.process_attribution_complete = false;
        }
    }

    fn drm_cards(&self) -> Vec<(GpuDevice, String)> {
        let Some(mut entries) = self.file_system.list(&self.drm_root) else {
            return Vec::new();
        };
        entries.sort();
        let mut cards = Vec::new();
        let mut seen = HashSet::new();
        for entry in entries.iter().filter(|e| is_card_entry(e)) {
            let device_path = format!("{}/{entry}/device", self.drm_root);
            let vendor = GpuVendor::from_pci_vendor_id(
                &self
                    .file_system
                    .read(&format!("{device_path}/vendor"))
                    .unwrap_or_default(),
            );
            if !matches!(vendor, GpuVendor::Amd | GpuVendor::Intel) {
                continue;
            }
            let uevent = parse_key_values(
                &self
                    .file_system
                    .read(&format!("{device_path}/uevent"))
                    .unwrap_or_default(),
            );
            let pdev = uevent
                .get("PCI_SLOT_NAME")
                .cloned()
                .unwrap_or_else(|| entry.clone());
            if !seen.insert(pdev.clone()) {
                continue;
            }
            let product = self
                .file_system
                .read(&format!("{device_path}/product_name"))
                .map(|p| p.trim().to_string());
            let name = product
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| format!("{} GPU {pdev}", vendor.display_name()));
            let mut device = GpuDevice::new(&format!("drm:{pdev}"), vendor, &name);
            if vendor == GpuVendor::Amd {
                let read = |f: &str| {
                    self.file_system
                        .read(&format!("{device_path}/{f}"))
                        .map(|v| v.trim().to_string())
                };
                device.utilization = read("gpu_busy_percent").and_then(|v| v.parse().ok());
                device.memory_used = read("mem_info_vram_used").and_then(|v| v.parse().ok());
                device.memory_total = read("mem_info_vram_total").and_then(|v| v.parse().ok());
            }
            cards.push((device, pdev));
        }
        cards
    }

    /// Scans every process's open DRM fds (what nvtop and intel_gpu_top do).
    pub fn collect_drm(&self, now: f64) -> GpuSample {
        let mut cards = self.drm_cards();
        if cards.is_empty() {
            return GpuSample {
                process_attribution_complete: true,
                ..Default::default()
            };
        }
        let mut clients: Vec<DrmClient> = Vec::new();
        let mut owner: HashMap<String, i64> = HashMap::new();
        let mut unreadable = 0;
        for entry in self.file_system.list(&self.proc_root).unwrap_or_default() {
            let Ok(pid) = entry.parse::<i64>() else {
                continue;
            };
            let fds = match self
                .file_system
                .list_access(&format!("{}/{pid}/fd", self.proc_root))
            {
                Access::Found(fds) => fds,
                Access::Denied => {
                    unreadable += 1;
                    continue;
                }
                // Exited during the scan.
                Access::Missing => continue,
            };
            // A process counts once however many of its fds were refused.
            let mut denied = false;
            for fd in fds {
                let target = match self
                    .file_system
                    .read_link_access(&format!("{}/{pid}/fd/{fd}", self.proc_root))
                {
                    Access::Found(target) => target,
                    Access::Denied => {
                        denied = true;
                        continue;
                    }
                    Access::Missing => continue,
                };
                if !target.starts_with("/dev/dri/") {
                    continue;
                }
                let text = match self
                    .file_system
                    .read_access(&format!("{}/{pid}/fdinfo/{fd}", self.proc_root))
                {
                    Access::Found(text) => text,
                    Access::Denied => {
                        denied = true;
                        continue;
                    }
                    Access::Missing => continue,
                };
                let Some(client) = DrmClient::parse(&text) else {
                    continue;
                };
                // One client can sit behind several fds (or processes after fork); count it once.
                if owner.contains_key(&client.key()) {
                    continue;
                }
                owner.insert(client.key(), pid);
                clients.push(client);
            }
            if denied {
                unreadable += 1;
            }
        }
        let usage = match self.tracker.lock() {
            Ok(mut tracker) => tracker.update(&clients, now),
            Err(_) => HashMap::new(),
        };
        let mut processes = Vec::new();
        let mut usage_by_device: HashMap<String, Vec<DrmClientUsage>> = HashMap::new();
        let mut memory_by_device: HashMap<String, u64> = HashMap::new();
        for client in &clients {
            let Some(&pid) = owner.get(&client.key()) else {
                continue;
            };
            let client_usage = usage.get(&client.key());
            if let Some(u) = client_usage {
                usage_by_device
                    .entry(client.pdev.clone())
                    .or_default()
                    .push(u.clone());
            }
            let memory = memory_by_device.entry(client.pdev.clone()).or_insert(0);
            *memory = memory.wrapping_add(client.device_memory());
            processes.push(GpuProcessUsage {
                pid,
                device_id: format!("drm:{}", client.pdev),
                utilization: client_usage.map(DrmClientUsage::busiest),
                memory_bytes: (client.device_memory() > 0).then(|| client.device_memory()),
                container_id: self.container_id(pid),
            });
        }
        if cards.iter().any(|(d, _)| d.vendor == GpuVendor::Amd) {
            let amd: HashSet<String> = cards
                .iter()
                .filter(|(d, _)| d.vendor == GpuVendor::Amd)
                .map(|(_, pdev)| pdev.clone())
                .collect();
            for process in self.collect_kfd(&amd, &mut unreadable) {
                merge_process(&mut processes, process);
            }
        }
        // Intel has no busy-percent file: device utilization and memory are summed from its
        // clients. With any process unreadable (any non-root run) that sum misses whatever those
        // processes do — containers, usually — so it is unknown rather than a low number.
        let has_intel = cards.iter().any(|(d, _)| d.vendor == GpuVendor::Intel);
        for (device, pdev) in cards
            .iter_mut()
            .filter(|(d, _)| d.vendor == GpuVendor::Intel && unreadable == 0)
        {
            let usages = usage_by_device
                .get(pdev.as_str())
                .cloned()
                .unwrap_or_default();
            device.utilization = Some(DrmUsageTracker::device_utilization(&usages).unwrap_or(0.0));
            if let Some(&memory) = memory_by_device.get(pdev.as_str()).filter(|m| **m > 0) {
                device.memory_used = Some(memory);
            }
        }
        let mut issues = Vec::new();
        if unreadable > 0 {
            issues.push(if has_intel {
                format!(
                    "Intel GPU usage and per-container GPU usage need root ({unreadable} processes were not readable)"
                )
            } else {
                format!(
                    "per-container AMD/Intel GPU usage needs root ({unreadable} processes were not readable)"
                )
            });
        }
        GpuSample {
            devices: cards.into_iter().map(|(d, _)| d).collect(),
            processes,
            process_attribution_complete: unreadable == 0,
            issues,
        }
    }

    /// ROCm (KFD) processes on the AMD GPUs in `amd` (PCI addresses): VRAM from
    /// `proc/<pid>/vram_<gpu_id>`, and utilization as the share of compute units the process
    /// occupies at this moment (`stats_<gpu_id>/cu_occupancy` over the GPU's CU count from the
    /// topology). The files are world-readable; a refused one counts as unreadable.
    fn collect_kfd(&self, amd: &HashSet<String>, unreadable: &mut usize) -> Vec<GpuProcessUsage> {
        let topology = format!("{}/topology/nodes", self.kfd_root);
        let mut gpus: HashMap<String, (String, Option<u64>)> = HashMap::new();
        for node in self.file_system.list(&topology).unwrap_or_default() {
            let read = |file: &str| self.file_system.read(&format!("{topology}/{node}/{file}"));
            let Some(gpu_id) = read("gpu_id").map(|g| g.trim().to_string()) else {
                continue;
            };
            if gpu_id.is_empty() || gpu_id == "0" {
                continue; // a CPU node
            }
            let properties = kfd_properties(&read("properties").unwrap_or_default());
            let Some(pdev) = kfd_pci_address(&properties) else {
                continue;
            };
            if !amd.contains(&pdev) {
                continue;
            }
            let compute_units = match (properties.get("simd_count"), properties.get("simd_per_cu"))
            {
                (Some(&simds), Some(&per_cu)) if per_cu > 0 && simds >= per_cu => {
                    Some(simds / per_cu)
                }
                _ => None,
            };
            gpus.insert(gpu_id, (pdev, compute_units));
        }
        if gpus.is_empty() {
            return Vec::new();
        }
        let proc_dir = format!("{}/proc", self.kfd_root);
        let pids = match self.file_system.list_access(&proc_dir) {
            Access::Found(pids) => pids,
            Access::Denied => {
                *unreadable += 1;
                return Vec::new();
            }
            Access::Missing => return Vec::new(),
        };
        let mut processes = Vec::new();
        for entry in pids {
            let Ok(pid) = entry.parse::<i64>() else {
                continue;
            };
            for (gpu_id, (pdev, compute_units)) in &gpus {
                let base = format!("{proc_dir}/{pid}");
                let vram = match self
                    .file_system
                    .read_access(&format!("{base}/vram_{gpu_id}"))
                {
                    Access::Found(v) => v.trim().parse::<u64>().ok(),
                    Access::Denied => {
                        *unreadable += 1;
                        None
                    }
                    // The process does not use this GPU (or exited).
                    Access::Missing => continue,
                };
                let occupied = self
                    .file_system
                    .read(&format!("{base}/stats_{gpu_id}/cu_occupancy"))
                    .and_then(|v| v.trim().parse::<u64>().ok());
                let utilization = match (occupied, compute_units) {
                    (Some(used), Some(total)) => {
                        Some((used as f64 / *total as f64 * 100.0).min(100.0))
                    }
                    _ => None,
                };
                processes.push(GpuProcessUsage {
                    pid,
                    device_id: format!("drm:{pdev}"),
                    utilization,
                    memory_bytes: vram,
                    container_id: self.container_id(pid),
                });
            }
        }
        processes
    }

    fn container_id(&self, pid: i64) -> Option<String> {
        self.file_system
            .read(&format!("{}/{pid}/cgroup", self.proc_root))
            .and_then(|t| cgroup_container_id(&t))
    }
}

impl GpuCollecting for SystemGpuCollector {
    fn sample(&self, now: f64) -> GpuSample {
        let mut result = GpuSample {
            process_attribution_complete: self.platform == Platform::Linux,
            ..Default::default()
        };
        if let Some(smi) = self.nvidia_smi.clone() {
            self.collect_nvidia(&smi, now, &mut result);
        }
        if self.platform == Platform::Linux {
            let drm = self.collect_drm(now);
            result.devices.extend(drm.devices);
            result.processes.extend(drm.processes);
            result.issues.extend(drm.issues);
            if !drm.process_attribution_complete {
                result.process_attribution_complete = false;
            }
        }
        if result.devices.is_empty() {
            result.process_attribution_complete = false;
        }
        result
    }
}

/// `key value` lines of a KFD topology `properties` file.
pub fn kfd_properties(text: &str) -> HashMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let key = parts.next()?;
            let value = parts.next()?.parse().ok()?;
            Some((key.to_string(), value))
        })
        .collect()
}

/// The PCI address (`0000:03:00.0`) of a KFD topology node: `domain` and `location_id`
/// (bus << 8 | device << 3 | function), as in `PCI_SLOT_NAME`.
pub fn kfd_pci_address(properties: &HashMap<String, u64>) -> Option<String> {
    let location = *properties.get("location_id")?;
    let domain = properties.get("domain").copied().unwrap_or(0);
    Some(format!(
        "{domain:04x}:{:02x}:{:02x}.{:x}",
        (location >> 8) & 0xff,
        (location >> 3) & 0x1f,
        location & 0x7
    ))
}

/// Adds `process` to `processes`, combining it with an entry for the same process and GPU
/// from another source (DRM fdinfo and KFD both see ROCm processes): the larger reading of
/// each wins, so nothing is counted twice.
fn merge_process(processes: &mut Vec<GpuProcessUsage>, process: GpuProcessUsage) {
    let larger_f = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    match processes
        .iter_mut()
        .find(|p| p.pid == process.pid && p.device_id == process.device_id)
    {
        Some(existing) => {
            existing.utilization = larger_f(existing.utilization, process.utilization);
            existing.memory_bytes = match (existing.memory_bytes, process.memory_bytes) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
        }
        None => processes.push(process),
    }
}

/// `card0` but not connector entries such as `card0-DP-1`.
pub fn is_card_entry(name: &str) -> bool {
    name.strip_prefix("card")
        .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
}

fn parse_key_values(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            l.split_once('=')
                .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// A registry property value, as much of CoreFoundation as the Apple parser needs.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Number(f64),
    String(String),
    Dictionary(BTreeMap<String, PropertyValue>),
}

/// Apple GPU statistics from `IOAccelerator` registry entries. `PerformanceStatistics` is
/// undocumented but readable without root; key names vary by GPU generation.
pub mod apple_statistics {
    use super::*;

    pub const UTILIZATION_KEYS: [&str; 2] = ["Device Utilization %", "GPU Activity(%)"];
    /// Apple Silicon reports unified-memory use; discrete GPUs in Intel Macs report VRAM.
    pub const MEMORY_KEYS: [&str; 2] = ["In use system memory", "vramUsedBytes"];

    /// Builds a device from one accelerator's properties (pure: tested with fixtures).
    pub fn device(index: usize, properties: &BTreeMap<String, PropertyValue>) -> Option<GpuDevice> {
        let Some(PropertyValue::Dictionary(stats)) = properties.get("PerformanceStatistics") else {
            return None;
        };
        let number = |key: &str| match stats.get(key) {
            Some(PropertyValue::Number(n)) => Some(*n),
            _ => None,
        };
        let utilization = UTILIZATION_KEYS.iter().find_map(|k| number(k));
        let memory = MEMORY_KEYS
            .iter()
            .find_map(|k| number(k))
            .map(|m| m.max(0.0) as u64);
        let name = ["model", "IOGLBundleName", "IOClass"]
            .iter()
            .find_map(|k| match properties.get(*k) {
                Some(PropertyValue::String(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "Apple GPU".into());
        let mut device = GpuDevice::new(&format!("apple:{index}"), GpuVendor::Apple, &name);
        device.utilization = utilization.map(|u| u.clamp(0.0, 100.0));
        device.memory_used = memory;
        Some(device)
    }
}

#[cfg(target_os = "macos")]
pub mod apple {
    //! Polls `IOAccelerator` services through IOKit. The frameworks are declared by hand (ADR
    //! 0001: no crates); only the handful of CoreFoundation calls the parser needs are bound.

    use super::*;
    use std::ffi::{c_char, c_void, CStr};

    type CFTypeRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;
    type IoObject = u32;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOServiceMatching(name: *const c_char) -> CFDictionaryRef;
        fn IOServiceGetMatchingServices(
            main_port: u32,
            matching: CFDictionaryRef,
            iterator: *mut IoObject,
        ) -> i32;
        fn IOIteratorNext(iterator: IoObject) -> IoObject;
        fn IOObjectRelease(object: IoObject) -> i32;
        fn IORegistryEntryCreateCFProperties(
            entry: IoObject,
            properties: *mut CFDictionaryRef,
            allocator: CFTypeRef,
            options: u32,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(value: CFTypeRef);
        fn CFGetTypeID(value: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFDictionaryGetTypeID() -> usize;
        fn CFDictionaryGetCount(dict: CFDictionaryRef) -> isize;
        fn CFDictionaryGetKeysAndValues(
            dict: CFDictionaryRef,
            keys: *mut CFTypeRef,
            values: *mut CFTypeRef,
        );
        fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> u8;
        fn CFStringGetCString(
            string: CFStringRef,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
    }

    const K_CF_NUMBER_DOUBLE_TYPE: isize = 13;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    /// SAFETY: `value` must be a valid CF object or null.
    unsafe fn convert(value: CFTypeRef, depth: usize) -> Option<PropertyValue> {
        if value.is_null() || depth > 2 {
            return None;
        }
        // SAFETY: the caller guarantees `value` is a live CF object.
        unsafe {
            let type_id = CFGetTypeID(value);
            if type_id == CFNumberGetTypeID() {
                let mut number = 0f64;
                (CFNumberGetValue(
                    value,
                    K_CF_NUMBER_DOUBLE_TYPE,
                    (&mut number as *mut f64).cast(),
                ) != 0)
                    .then_some(PropertyValue::Number(number))
            } else if type_id == CFStringGetTypeID() {
                let mut buffer = [0 as c_char; 512];
                (CFStringGetCString(
                    value,
                    buffer.as_mut_ptr(),
                    buffer.len() as isize,
                    K_CF_STRING_ENCODING_UTF8,
                ) != 0)
                    .then(|| {
                        PropertyValue::String(
                            CStr::from_ptr(buffer.as_ptr())
                                .to_string_lossy()
                                .into_owned(),
                        )
                    })
            } else if type_id == CFDictionaryGetTypeID() {
                Some(PropertyValue::Dictionary(convert_dictionary(
                    value,
                    depth + 1,
                )))
            } else {
                None
            }
        }
    }

    /// SAFETY: `dict` must be a valid CFDictionary.
    unsafe fn convert_dictionary(
        dict: CFDictionaryRef,
        depth: usize,
    ) -> BTreeMap<String, PropertyValue> {
        let mut out = BTreeMap::new();
        // SAFETY: `dict` is a live CFDictionary; the key/value arrays are sized from its count.
        unsafe {
            let count = CFDictionaryGetCount(dict).max(0) as usize;
            let mut keys = vec![std::ptr::null::<c_void>(); count];
            let mut values = vec![std::ptr::null::<c_void>(); count];
            CFDictionaryGetKeysAndValues(dict, keys.as_mut_ptr(), values.as_mut_ptr());
            for (key, value) in keys.into_iter().zip(values) {
                let Some(PropertyValue::String(name)) = convert(key, 0) else {
                    continue;
                };
                if let Some(converted) = convert(value, depth) {
                    out.insert(name, converted);
                }
            }
        }
        out
    }

    pub struct AppleGpuCollector;

    impl GpuCollecting for AppleGpuCollector {
        fn sample(&self, _now: f64) -> GpuSample {
            let mut devices = Vec::new();
            let mut iterator: IoObject = 0;
            // SAFETY: IOKit calls with valid arguments; every object we obtain is released.
            unsafe {
                let matching = IOServiceMatching(c"IOAccelerator".as_ptr());
                if IOServiceGetMatchingServices(0, matching, &mut iterator) != 0 {
                    return GpuSample {
                        issues: vec!["could not query IOAccelerator services".into()],
                        ..Default::default()
                    };
                }
                loop {
                    let service = IOIteratorNext(iterator);
                    if service == 0 {
                        break;
                    }
                    let mut properties: CFDictionaryRef = std::ptr::null();
                    if IORegistryEntryCreateCFProperties(
                        service,
                        &mut properties,
                        std::ptr::null(),
                        0,
                    ) == 0
                        && !properties.is_null()
                    {
                        let converted = convert_dictionary(properties, 0);
                        CFRelease(properties);
                        if let Some(device) = apple_statistics::device(devices.len(), &converted) {
                            devices.push(device);
                        }
                    }
                    IOObjectRelease(service);
                }
                IOObjectRelease(iterator);
            }
            // Containers on macOS run in Docker Desktop's Linux VM, which has no GPU access, so
            // nothing is attributed per container.
            GpuSample {
                devices,
                ..Default::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::CommandResult;
    use crate::error::Result;

    #[test]
    fn finds_container_ids_in_cgroups() {
        let id = "a".repeat(64);
        assert_eq!(
            cgroup_container_id(&format!("0::/system.slice/docker-{id}.scope\n")),
            Some(id.clone())
        );
        assert_eq!(
            cgroup_container_id(&format!("12:memory:/docker/{id}\n")),
            Some(id)
        );
        assert_eq!(cgroup_container_id("0::/user.slice\n"), None);
    }

    #[test]
    fn parses_nvidia_smi_output() {
        let devices = nvidia_smi::parse_devices(
            "0, GPU-abc, NVIDIA RTX, 45, 1024, 2048, 61\n1, GPU-def, X, [N/A], 0, 0, [N/A]\n",
        );
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].device.utilization, Some(45.0));
        assert_eq!(devices[0].device.memory_used, Some(1024 * 1_048_576));
        assert_eq!(devices[1].device.utilization, None);
        let pmon = nvidia_smi::parse_process_monitor(
            "# gpu pid type sm mem enc dec command\n# Idx # C/G % % % % name\n    0   4242   C   45  12  -  -  python\n    0   99  C  -  -  -  -  x\n",
        );
        let row = |gpu, pid, sm, memory| nvidia_smi::ProcessSample {
            gpu,
            pid,
            sm,
            memory,
        };
        assert_eq!(
            pmon,
            vec![row(0, 4242, Some(45.0), None), row(0, 99, None, None)]
        );
        // `-s um` on a current driver: extra columns, and fb (MiB) for a graphics process.
        let pmon = nvidia_smi::parse_process_monitor(
            "# gpu         pid   type     sm    mem    enc    dec    jpg    ofa     fb   ccpm    command\n\
             # Idx           #    C/G      %      %      %      %      %      %     MB     MB    name\n\
                 0       1234     G      3      1      -      -      -      -     45      0    Xorg\n\
                 0       4242     C     60     20      -      -      -      -   2048      0    python\n",
        );
        assert_eq!(
            pmon,
            vec![
                row(0, 1234, Some(3.0), Some(45 * 1_048_576)),
                row(0, 4242, Some(60.0), Some(2048 * 1_048_576)),
            ]
        );
        assert_eq!(
            nvidia_smi::parse_compute_apps("4242, GPU-abc, 1536\n"),
            vec![(4242, "GPU-abc".into(), Some(1536 * 1_048_576))]
        );
    }

    #[test]
    fn drm_tracker_computes_engine_shares() {
        let text = |ns: u64| {
            format!("drm-driver: amdgpu\ndrm-client-id: 7\ndrm-pdev: 0000:03:00.0\ndrm-engine-gfx: {ns} ns\ndrm-memory-vram: 16 MiB\n")
        };
        let mut tracker = DrmUsageTracker::default();
        let first = DrmClient::parse(&text(1_000_000_000)).unwrap();
        assert_eq!(first.device_memory(), 16 * 1_048_576);
        assert!(tracker.update(&[first], 0.0).is_empty());
        let second = DrmClient::parse(&text(1_500_000_000)).unwrap();
        let usage = tracker.update(std::slice::from_ref(&second), 1.0);
        assert!((usage[&second.key()].busiest() - 50.0).abs() < 1e-9);
        assert_eq!(
            DrmUsageTracker::device_utilization(&[
                usage[&second.key()].clone(),
                usage[&second.key()].clone()
            ]),
            Some(100.0)
        );
    }

    #[test]
    fn xe_cycles_are_supported() {
        let text = |c: u64, t: u64| {
            format!("drm-driver: xe\ndrm-client-id: 1\ndrm-cycles-rcs: {c}\ndrm-total-cycles-rcs: {t}\n")
        };
        let mut tracker = DrmUsageTracker::default();
        tracker.update(&[DrmClient::parse(&text(0, 1000)).unwrap()], 0.0);
        let c = DrmClient::parse(&text(250, 2000)).unwrap();
        let usage = tracker.update(std::slice::from_ref(&c), 1.0);
        assert!((usage[&c.key()].busiest() - 25.0).abs() < 1e-9);
    }

    #[test]
    fn usage_by_container_averages_across_devices() {
        let sample = GpuSample {
            devices: vec![
                GpuDevice::new("a", GpuVendor::Nvidia, "a"),
                GpuDevice::new("b", GpuVendor::Nvidia, "b"),
            ],
            processes: vec![GpuProcessUsage {
                pid: 1,
                device_id: "a".into(),
                utilization: Some(100.0),
                memory_bytes: Some(5),
                container_id: Some("c".into()),
            }],
            ..Default::default()
        };
        assert_eq!(sample.usage_by_container()["c"], (50.0, 5));
    }

    struct FakeFs(BTreeMap<String, String>, BTreeMap<String, String>);

    /// A `FakeFs` where the listed paths are refused, as `/proc` does for other users.
    struct DenyingFs(FakeFs, std::collections::BTreeSet<String>);

    impl GpuFileSystem for DenyingFs {
        fn read(&self, path: &str) -> Option<String> {
            self.0.read(path)
        }
        fn list(&self, path: &str) -> Option<Vec<String>> {
            self.0.list(path)
        }
        fn read_link(&self, path: &str) -> Option<String> {
            self.0.read_link(path)
        }
        fn list_access(&self, path: &str) -> Access<Vec<String>> {
            if self.1.contains(path) {
                return Access::Denied;
            }
            Access::from_option(self.0.list(path))
        }
        fn read_link_access(&self, path: &str) -> Access<String> {
            if self.1.contains(path) {
                return Access::Denied;
            }
            Access::from_option(self.0.read_link(path))
        }
        fn read_access(&self, path: &str) -> Access<String> {
            if self.1.contains(path) {
                return Access::Denied;
            }
            Access::from_option(self.0.read(path))
        }
    }

    impl GpuFileSystem for FakeFs {
        fn read(&self, path: &str) -> Option<String> {
            self.0.get(path).cloned()
        }
        fn list(&self, path: &str) -> Option<Vec<String>> {
            let prefix = format!("{path}/");
            let mut names: Vec<String> = self
                .0
                .keys()
                .chain(self.1.keys())
                .filter_map(|k| {
                    k.strip_prefix(&prefix)
                        .map(|r| r.split('/').next().unwrap_or("").to_string())
                })
                .collect();
            names.dedup();
            (!names.is_empty()).then_some(names)
        }
        fn read_link(&self, path: &str) -> Option<String> {
            self.1.get(path).cloned()
        }
    }

    struct NoRunner;
    impl CommandRunning for NoRunner {
        fn run(&self, _: &CommandInvocation) -> Result<CommandResult> {
            Ok(CommandResult {
                exit_code: 1,
                output: "no".into(),
            })
        }
    }

    #[test]
    fn collects_intel_usage_from_fdinfo_and_attributes_containers() {
        let id = "b".repeat(64);
        let fdinfo = |ns: u64| {
            format!("drm-driver: i915\ndrm-client-id: 3\ndrm-pdev: 0000:00:02.0\ndrm-engine-render: {ns} ns\n")
        };
        let mut files: BTreeMap<String, String> = BTreeMap::new();
        files.insert(
            "/sys/class/drm/card0/device/vendor".into(),
            "0x8086\n".into(),
        );
        files.insert(
            "/sys/class/drm/card0/device/uevent".into(),
            "PCI_SLOT_NAME=0000:00:02.0\n".into(),
        );
        files.insert("/sys/class/drm/card0-DP-1/x".into(), String::new());
        files.insert("/proc/42/fdinfo/5".into(), fdinfo(0));
        files.insert("/proc/42/cgroup".into(), format!("0::/docker/{id}\n"));
        let links: BTreeMap<String, String> = [(
            "/proc/42/fd/5".to_string(),
            "/dev/dri/renderD128".to_string(),
        )]
        .into();
        let collector = SystemGpuCollector::new(
            Platform::Linux,
            None,
            Arc::new(FakeFs(files.clone(), links.clone())),
            Arc::new(NoRunner),
            "/proc",
            "/sys/class/drm",
        );
        collector.sample(0.0);
        files.insert("/proc/42/fdinfo/5".into(), fdinfo(250_000_000));
        let collector2 = SystemGpuCollector {
            file_system: Arc::new(FakeFs(files, links)),
            ..collector
        };
        let sample = collector2.sample(1.0);
        assert_eq!(sample.devices.len(), 1);
        assert_eq!(sample.devices[0].name, "Intel GPU 0000:00:02.0");
        assert!((sample.devices[0].utilization.unwrap() - 25.0).abs() < 1e-9);
        assert_eq!(
            sample.processes[0].container_id.as_deref(),
            Some(id.as_str())
        );
        assert!(sample.process_attribution_complete);
    }

    /// `nvidia-smi` that answers device queries from `devices` and hangs (times out) while
    /// `hung` is set. Counts the queries it receives.
    #[derive(Default)]
    struct ScriptedSmi {
        hung: std::sync::atomic::AtomicBool,
        /// Only `pmon` hangs.
        pmon_hung: std::sync::atomic::AtomicBool,
        calls: std::sync::atomic::AtomicUsize,
        abandoned: std::sync::atomic::AtomicUsize,
        /// `--query-compute-apps` and `pmon` output.
        apps: String,
        pmon: String,
    }

    impl CommandRunning for ScriptedSmi {
        fn run(&self, _: &CommandInvocation) -> Result<CommandResult> {
            unreachable!("nvidia-smi queries must use run_with_timeout")
        }
        fn run_with_timeout(
            &self,
            invocation: &CommandInvocation,
            _: Duration,
        ) -> Result<CommandResult> {
            use std::sync::atomic::Ordering;
            self.calls.fetch_add(1, Ordering::SeqCst);
            let pmon = invocation.arguments[0] == "pmon";
            if self.hung.load(Ordering::SeqCst) || (pmon && self.pmon_hung.load(Ordering::SeqCst)) {
                return Err(DockerError::TimedOut(invocation.display_string()));
            }
            let argument = invocation.arguments[0].as_str();
            let output = if argument.starts_with("--query-gpu") {
                "0, GPU-abc, NVIDIA RTX, 45, 1024, 2048, 61\n"
            } else if argument.starts_with("--query-compute-apps") {
                self.apps.as_str()
            } else {
                self.pmon.as_str()
            };
            Ok(CommandResult {
                exit_code: 0,
                output: output.into(),
            })
        }
        fn abandoned_processes(&self) -> usize {
            self.abandoned.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn nvidia_collector(smi: &Arc<ScriptedSmi>, fs: FakeFs) -> SystemGpuCollector {
        SystemGpuCollector::new(
            Platform::Linux,
            Some("/usr/bin/nvidia-smi".into()),
            Arc::new(fs),
            Arc::clone(smi) as Arc<dyn CommandRunning>,
            "/proc",
            "/sys/class/drm",
        )
    }

    #[test]
    fn partial_nvidia_answers_leave_containers_unknown_not_idle() {
        use std::sync::atomic::Ordering;
        // pmon times out after the device query worked.
        let smi = Arc::new(ScriptedSmi::default());
        smi.pmon_hung.store(true, Ordering::SeqCst);
        let collector = nvidia_collector(&smi, FakeFs(BTreeMap::new(), BTreeMap::new()));
        let sample = collector.sample(0.0);
        assert_eq!(sample.devices[0].utilization, Some(45.0));
        assert!(!sample.process_attribution_complete);

        // The very first query times out on a host that also has an AMD GPU, whose DRM data is
        // complete: NVIDIA usage is unknown, so attribution is incomplete.
        let mut files: BTreeMap<String, String> = BTreeMap::new();
        files.insert(
            "/sys/class/drm/card0/device/vendor".into(),
            "0x1002\n".into(),
        );
        files.insert(
            "/sys/class/drm/card0/device/uevent".into(),
            "PCI_SLOT_NAME=0000:03:00.0\n".into(),
        );
        files.insert(
            "/sys/class/drm/card0/device/gpu_busy_percent".into(),
            "7\n".into(),
        );
        let smi = Arc::new(ScriptedSmi::default());
        smi.hung.store(true, Ordering::SeqCst);
        let collector = nvidia_collector(&smi, FakeFs(files, BTreeMap::new()));
        let sample = collector.sample(0.0);
        assert_eq!(sample.devices.len(), 1);
        assert_eq!(sample.devices[0].utilization, Some(7.0));
        assert!(!sample.process_attribution_complete);
    }

    #[test]
    fn graphics_processes_get_memory_from_pmon_and_compute_ones_keep_the_exact_figure() {
        let id = "c".repeat(64);
        let mut files = BTreeMap::new();
        files.insert(
            "/proc/1234/cgroup".to_string(),
            format!("0::/docker/{id}\n"),
        );
        let smi = Arc::new(ScriptedSmi {
            apps: "4242, GPU-abc, 1536\n".into(),
            pmon: "# gpu  pid  type  sm  mem  enc  dec  jpg  ofa  fb  ccpm  command\n\
                   # Idx    #   C/G   %    %    %    %    %    %  MB    MB  name\n\
                       0  1234    G   3    1    -    -    -    -  45     0  glxgears\n\
                       0  4242    C  60   20    -    -    -    -  2000   0  python\n"
                .into(),
            ..ScriptedSmi::default()
        });
        let collector = nvidia_collector(&smi, FakeFs(files, BTreeMap::new()));
        let sample = collector.sample(0.0);
        let process = |pid| sample.processes.iter().find(|p| p.pid == pid).unwrap();
        // A graphics process --query-compute-apps does not list: memory from pmon's fb.
        assert_eq!(process(1234).memory_bytes, Some(45 * 1_048_576));
        assert_eq!(process(1234).utilization, Some(3.0));
        assert_eq!(process(1234).container_id.as_deref(), Some(id.as_str()));
        // A compute process keeps --query-compute-apps' exact figure.
        assert_eq!(process(4242).memory_bytes, Some(1536 * 1_048_576));
        assert_eq!(process(4242).utilization, Some(60.0));
        assert_eq!(sample.usage_by_container()[&id], (3.0, 45 * 1_048_576));
    }

    /// An AMD GPU at 0000:03:00.0 with 76 compute units, whose KFD process 77 (in a container)
    /// uses 2 GiB of VRAM and occupies 19 CUs.
    fn amd_kfd_files(id: &str) -> BTreeMap<String, String> {
        let mut files = BTreeMap::new();
        let card = "/sys/class/drm/card1/device";
        files.insert(format!("{card}/vendor"), "0x1002\n".into());
        files.insert(
            format!("{card}/uevent"),
            "PCI_SLOT_NAME=0000:03:00.0\n".into(),
        );
        files.insert(format!("{card}/gpu_busy_percent"), "40\n".into());
        let kfd = "/sys/class/kfd/kfd";
        files.insert(format!("{kfd}/topology/nodes/0/gpu_id"), "0\n".into());
        files.insert(
            format!("{kfd}/topology/nodes/0/properties"),
            "cpu_cores_count 16\n".into(),
        );
        files.insert(format!("{kfd}/topology/nodes/1/gpu_id"), "53418\n".into());
        files.insert(
            format!("{kfd}/topology/nodes/1/properties"),
            "simd_count 304\nsimd_per_cu 4\nlocation_id 768\ndomain 0\n".into(),
        );
        files.insert(format!("{kfd}/proc/77/vram_53418"), "2147483648\n".into());
        files.insert(
            format!("{kfd}/proc/77/stats_53418/cu_occupancy"),
            "19\n".into(),
        );
        files.insert("/proc/77/cgroup".to_string(), format!("0::/docker/{id}\n"));
        files
    }

    #[test]
    fn rocm_processes_are_read_from_kfd() {
        let id = "d".repeat(64);
        let collector = SystemGpuCollector::new(
            Platform::Linux,
            None,
            Arc::new(FakeFs(amd_kfd_files(&id), BTreeMap::new())),
            Arc::new(NoRunner),
            "/proc",
            "/sys/class/drm",
        );
        let sample = collector.sample(0.0);
        assert_eq!(sample.devices.len(), 1);
        assert_eq!(sample.devices[0].utilization, Some(40.0));
        assert_eq!(sample.processes.len(), 1);
        let p = &sample.processes[0];
        assert_eq!(p.pid, 77);
        assert_eq!(p.device_id, "drm:0000:03:00.0");
        assert_eq!(p.memory_bytes, Some(2 * 1024 * 1_048_576));
        assert!((p.utilization.unwrap() - 25.0).abs() < 1e-9);
        assert!(sample.process_attribution_complete);
        let (utilization, memory) = sample.usage_by_container()[&id];
        assert!((utilization - 25.0).abs() < 1e-9);
        assert_eq!(memory, 2 * 1024 * 1_048_576);
    }

    #[test]
    fn a_process_seen_by_both_fdinfo_and_kfd_is_counted_once() {
        let id = "e".repeat(64);
        let mut files = amd_kfd_files(&id);
        // The same process also holds the render node, with 16 MiB of VRAM there.
        files.insert(
            "/proc/77/fdinfo/9".into(),
            "drm-driver: amdgpu\ndrm-client-id: 5\ndrm-pdev: 0000:03:00.0\ndrm-engine-gfx: 0 ns\ndrm-memory-vram: 16 MiB\n".into(),
        );
        let links: BTreeMap<String, String> = [(
            "/proc/77/fd/9".to_string(),
            "/dev/dri/renderD129".to_string(),
        )]
        .into();
        let collector = SystemGpuCollector::new(
            Platform::Linux,
            None,
            Arc::new(FakeFs(files, links)),
            Arc::new(NoRunner),
            "/proc",
            "/sys/class/drm",
        );
        let sample = collector.sample(0.0);
        assert_eq!(sample.processes.len(), 1, "{:?}", sample.processes);
        assert_eq!(sample.processes[0].memory_bytes, Some(2 * 1024 * 1_048_576));
        assert!((sample.processes[0].utilization.unwrap() - 25.0).abs() < 1e-9);
    }

    #[test]
    fn kfd_topology_gives_pci_addresses() {
        let properties = kfd_properties("location_id 768\ndomain 0\nsimd_count 304\n");
        assert_eq!(
            kfd_pci_address(&properties).as_deref(),
            Some("0000:03:00.0")
        );
        // bus 0xc1, device 0, function 1, domain 1.
        let properties = kfd_properties(&format!("location_id {}\ndomain 1\n", (0xc1 << 8) | 1));
        assert_eq!(
            kfd_pci_address(&properties).as_deref(),
            Some("0001:c1:00.1")
        );
        assert_eq!(kfd_pci_address(&kfd_properties("simd_count 4\n")), None);
    }

    #[test]
    fn no_new_nvidia_smi_starts_while_a_killed_one_is_stuck() {
        use std::sync::atomic::Ordering;
        let smi = Arc::new(ScriptedSmi::default());
        let collector = nvidia_collector(&smi, FakeFs(BTreeMap::new(), BTreeMap::new()));
        collector.sample(0.0);
        smi.hung.store(true, Ordering::SeqCst);
        collector.sample(1.0);
        let calls = smi.calls.load(Ordering::SeqCst);
        // The killed process never exits (uninterruptible sleep in the driver).
        smi.abandoned.store(1, Ordering::SeqCst);
        smi.hung.store(false, Ordering::SeqCst);
        let stuck = collector.sample(1.0 + NVIDIA_SMI_RETRY_SECONDS + 5.0);
        assert_eq!(smi.calls.load(Ordering::SeqCst), calls);
        assert!(
            stuck.issues[0].contains("still stuck"),
            "{:?}",
            stuck.issues
        );
        assert_eq!(stuck.devices[0].utilization, None);
        // Once it exits, the next refresh queries again.
        smi.abandoned.store(0, Ordering::SeqCst);
        let recovered = collector.sample(1.0 + NVIDIA_SMI_RETRY_SECONDS + 7.0);
        assert_eq!(recovered.devices[0].utilization, Some(45.0));
    }

    #[test]
    fn a_hung_nvidia_smi_is_stopped_and_left_alone_until_the_retry() {
        use std::sync::atomic::Ordering;
        let smi = Arc::new(ScriptedSmi::default());
        let collector = SystemGpuCollector::new(
            Platform::Linux,
            Some("/usr/bin/nvidia-smi".into()),
            Arc::new(FakeFs(BTreeMap::new(), BTreeMap::new())),
            Arc::clone(&smi) as Arc<dyn CommandRunning>,
            "/proc",
            "/sys/class/drm",
        );
        let healthy = collector.sample(0.0);
        assert_eq!(healthy.devices[0].utilization, Some(45.0));
        assert_eq!(smi.calls.load(Ordering::SeqCst), 3);

        // The driver hangs: one timed-out query, then the GPU stays listed without readings.
        smi.hung.store(true, Ordering::SeqCst);
        let hung = collector.sample(2.0);
        assert_eq!(smi.calls.load(Ordering::SeqCst), 4);
        assert_eq!(hung.devices.len(), 1);
        assert_eq!(hung.devices[0].name, "NVIDIA RTX");
        assert_eq!(hung.devices[0].utilization, None);
        assert_eq!(hung.devices[0].memory_total, Some(2048 * 1_048_576));
        assert!(!hung.process_attribution_complete);
        assert!(
            hung.issues[0].contains("did not answer within 5 s"),
            "{:?}",
            hung.issues
        );

        // Until the retry time nvidia-smi is not run at all.
        let waiting = collector.sample(2.0 + NVIDIA_SMI_RETRY_SECONDS - 1.0);
        assert_eq!(smi.calls.load(Ordering::SeqCst), 4);
        assert!(
            waiting.issues[0].contains("retrying in 1 s"),
            "{:?}",
            waiting.issues
        );
        assert_eq!(waiting.devices[0].utilization, None);

        // Then it is tried again and recovers.
        smi.hung.store(false, Ordering::SeqCst);
        let recovered = collector.sample(2.0 + NVIDIA_SMI_RETRY_SECONDS);
        assert_eq!(smi.calls.load(Ordering::SeqCst), 7);
        assert_eq!(recovered.devices[0].utilization, Some(45.0));
        assert!(recovered.issues.is_empty(), "{:?}", recovered.issues);
    }

    /// Two samples one second apart of an Intel GPU whose client (pid 42) spends 25% of that
    /// second busy, with `denied` paths refused and `/proc/<extra>` processes listed.
    fn intel_sample(denied: &[&str], extra: &[&str]) -> GpuSample {
        let fdinfo = |ns: u64| {
            format!("drm-driver: i915\ndrm-client-id: 3\ndrm-pdev: 0000:00:02.0\ndrm-engine-render: {ns} ns\ndrm-resident-local0: 4 MiB\n")
        };
        let mut files: BTreeMap<String, String> = BTreeMap::new();
        files.insert(
            "/sys/class/drm/card0/device/vendor".into(),
            "0x8086\n".into(),
        );
        files.insert(
            "/sys/class/drm/card0/device/uevent".into(),
            "PCI_SLOT_NAME=0000:00:02.0\n".into(),
        );
        for pid in extra {
            files.insert(format!("/proc/{pid}/cgroup"), "0::/init.scope\n".into());
        }
        let links: BTreeMap<String, String> = [(
            "/proc/42/fd/5".to_string(),
            "/dev/dri/renderD128".to_string(),
        )]
        .into();
        let denied: std::collections::BTreeSet<String> =
            denied.iter().map(|d| d.to_string()).collect();
        let at = |ns| {
            let mut files = files.clone();
            files.insert("/proc/42/fdinfo/5".into(), fdinfo(ns));
            Arc::new(DenyingFs(FakeFs(files, links.clone()), denied.clone()))
        };
        let collector = SystemGpuCollector::new(
            Platform::Linux,
            None,
            at(0),
            Arc::new(NoRunner),
            "/proc",
            "/sys/class/drm",
        );
        collector.sample(0.0);
        let collector = SystemGpuCollector {
            file_system: at(250_000_000),
            ..collector
        };
        collector.sample(1.0)
    }

    fn assert_intel_unknown(sample: &GpuSample) {
        assert_eq!(sample.devices.len(), 1);
        // Only the readable client's 25% was visible; the device total is unknown, not 25%.
        assert_eq!(sample.devices[0].utilization, None);
        assert_eq!(sample.devices[0].memory_used, None);
        assert!(!sample.process_attribution_complete);
        assert!(
            sample.issues[0].starts_with("Intel GPU usage and per-container GPU usage need root"),
            "{:?}",
            sample.issues
        );
    }

    #[test]
    fn intel_usage_is_unknown_when_some_processes_cannot_be_read() {
        // Another user's process whose fd directory is refused.
        assert_intel_unknown(&intel_sample(&["/proc/1/fd"], &["1"]));
        // A listable fd directory whose DRM fdinfo is refused.
        assert_intel_unknown(&intel_sample(&["/proc/42/fdinfo/5"], &[]));
        // A refused fd link.
        assert_intel_unknown(&intel_sample(&["/proc/42/fd/5"], &[]));
    }

    #[test]
    fn processes_that_exit_during_the_scan_do_not_make_intel_usage_unknown() {
        // Process 7 is listed but its fd directory is gone by the time it is read.
        let sample = intel_sample(&[], &["7"]);
        assert!((sample.devices[0].utilization.unwrap() - 25.0).abs() < 1e-9);
        assert_eq!(sample.devices[0].memory_used, Some(4 * 1_048_576));
        assert!(sample.process_attribution_complete);
        assert!(sample.issues.is_empty(), "{:?}", sample.issues);
    }

    #[test]
    fn apple_statistics_pick_known_keys() {
        let mut stats = BTreeMap::new();
        stats.insert(
            "Device Utilization %".to_string(),
            PropertyValue::Number(37.0),
        );
        stats.insert(
            "In use system memory".to_string(),
            PropertyValue::Number(1024.0),
        );
        let mut props = BTreeMap::new();
        props.insert(
            "PerformanceStatistics".to_string(),
            PropertyValue::Dictionary(stats),
        );
        props.insert(
            "model".to_string(),
            PropertyValue::String("Apple M3".into()),
        );
        let d = apple_statistics::device(0, &props).unwrap();
        assert_eq!(
            (d.name.as_str(), d.utilization, d.memory_used),
            ("Apple M3", Some(37.0), Some(1024))
        );
        assert!(apple_statistics::device(0, &BTreeMap::new()).is_none());
    }
}
