//! GPU monitoring (ADR 0002): NVIDIA via `nvidia-smi` (Linux, Windows), AMD and Intel via
//! sysfs and DRM fdinfo (Linux), Apple via IOKit (macOS). Per-process usage is attributed to
//! containers through `/proc/<pid>/cgroup`, which only works on the Docker host itself.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::compose::{resolve_executable, CommandInvocation, CommandRunning, ProcessCommandRunner};
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
    pub const PROCESS_MONITOR_ARGUMENTS: [&str; 5] = ["pmon", "-c", "1", "-s", "u"];

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

    /// `nvidia-smi pmon -c 1 -s u` → (gpu index, pid, SM utilization); `-` means no sample.
    pub fn parse_process_monitor(output: &str) -> Vec<(i64, i64, Option<f64>)> {
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
            let position = |name: &str, default: usize| {
                columns.iter().position(|c| c == name).unwrap_or(default)
            };
            let (gpu, pid, sm) = (position("gpu", 0), position("pid", 1), position("sm", 3));
            if fields.len() <= gpu.max(pid).max(sm) {
                continue;
            }
            let (Ok(index), Ok(p)) = (fields[gpu].parse(), fields[pid].parse()) else {
                continue;
            };
            rows.push((index, p, number(fields[sm])));
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

/// File access the collector needs, injectable for tests against a fake `/sys` + `/proc`.
pub trait GpuFileSystem: Send + Sync {
    fn read(&self, path: &str) -> Option<String>;
    fn list(&self, path: &str) -> Option<Vec<String>>;
    fn read_link(&self, path: &str) -> Option<String>;
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
    tracker: Mutex<DrmUsageTracker>,
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
            tracker: Mutex::new(DrmUsageTracker::default()),
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

    fn run(&self, executable: &str, arguments: &[&str], result: &mut GpuSample) -> Option<String> {
        let invocation = CommandInvocation {
            executable: executable.into(),
            arguments: arguments.iter().map(|a| a.to_string()).collect(),
            working_directory: None,
        };
        match self.runner.run(&invocation) {
            Ok(output) if output.succeeded() => Some(output.output),
            Ok(output) => {
                let reason = output
                    .output
                    .lines()
                    .next()
                    .map_or_else(|| format!("exit code {}", output.exit_code), str::to_string);
                result.issues.push(format!("nvidia-smi failed: {reason}"));
                None
            }
            Err(error) => {
                result
                    .issues
                    .push(format!("nvidia-smi could not run: {error}"));
                None
            }
        }
    }

    fn collect_nvidia(&self, executable: &str, result: &mut GpuSample) {
        let Some(output) = self.run(executable, &nvidia_smi::DEVICE_QUERY_ARGUMENTS, result) else {
            return;
        };
        let devices = nvidia_smi::parse_devices(&output);
        if devices.is_empty() {
            return;
        }
        result
            .devices
            .extend(devices.iter().map(|d| d.device.clone()));
        let apps = self
            .run(executable, &nvidia_smi::COMPUTE_APPS_ARGUMENTS, result)
            .map(|o| nvidia_smi::parse_compute_apps(&o))
            .unwrap_or_default();
        // pmon is optional (unsupported on some GPUs); without it only per-process % is lost.
        let invocation = CommandInvocation {
            executable: executable.into(),
            arguments: nvidia_smi::PROCESS_MONITOR_ARGUMENTS
                .iter()
                .map(|a| a.to_string())
                .collect(),
            working_directory: None,
        };
        let monitor = match self.runner.run(&invocation) {
            Ok(o) if o.succeeded() => nvidia_smi::parse_process_monitor(&o.output),
            _ => Vec::new(),
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
        for (index, pid, utilization) in monitor {
            let Some(uuid) = uuid_by_index.get(&index) else {
                continue;
            };
            let entry = usage
                .entry(format!("{uuid}#{pid}"))
                .or_insert_with(|| GpuProcessUsage {
                    pid,
                    device_id: format!("nvidia:{uuid}"),
                    utilization: None,
                    memory_bytes: None,
                    container_id: None,
                });
            entry.utilization = utilization;
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
            let Some(fds) = self
                .file_system
                .list(&format!("{}/{pid}/fd", self.proc_root))
            else {
                unreadable += 1;
                continue;
            };
            for fd in fds {
                let Some(target) = self
                    .file_system
                    .read_link(&format!("{}/{pid}/fd/{fd}", self.proc_root))
                else {
                    continue;
                };
                if !target.starts_with("/dev/dri/") {
                    continue;
                }
                let Some(text) = self
                    .file_system
                    .read(&format!("{}/{pid}/fdinfo/{fd}", self.proc_root))
                else {
                    continue;
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
        // Intel has no busy-percent file: derive device utilization from its clients.
        for (device, pdev) in cards
            .iter_mut()
            .filter(|(d, _)| d.vendor == GpuVendor::Intel)
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
            issues.push(format!(
                "per-container AMD/Intel GPU usage needs root ({unreadable} processes were not readable)"
            ));
        }
        GpuSample {
            devices: cards.into_iter().map(|(d, _)| d).collect(),
            processes,
            process_attribution_complete: unreadable == 0,
            issues,
        }
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
            self.collect_nvidia(&smi, &mut result);
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
        assert_eq!(pmon, vec![(0, 4242, Some(45.0)), (0, 99, None)]);
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
        let usage = tracker.update(&[second.clone()], 1.0);
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
        let usage = tracker.update(&[c.clone()], 1.0);
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
