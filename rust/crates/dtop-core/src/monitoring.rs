//! Metrics math: rates and percentages from cumulative counters, ring-buffer history, reconnect
//! backoff, and process trees from `docker top`.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::gpu::GpuSample;
use crate::models::{ContainerStats, ContainerTop, CpuStats};
use crate::timestamp;

/// Exponential reconnect backoff with an upper bound and optional jitter.
#[derive(Debug, Clone, PartialEq)]
pub struct Backoff {
    pub initial_delay: f64,
    pub maximum_delay: f64,
    pub multiplier: f64,
    /// 0 disables jitter; 0.2 means ±20%.
    pub jitter: f64,
    pub attempt: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff::new(0.5, 30.0, 2.0, 0.2)
    }
}

impl Backoff {
    pub fn new(initial_delay: f64, maximum_delay: f64, multiplier: f64, jitter: f64) -> Self {
        assert!(
            initial_delay > 0.0 && maximum_delay >= initial_delay && multiplier >= 1.0,
            "invalid backoff configuration"
        );
        Backoff {
            initial_delay,
            maximum_delay,
            multiplier,
            jitter: jitter.clamp(0.0, 1.0),
            attempt: 0,
        }
    }

    /// Delay for the current attempt before jitter.
    pub fn base_delay(&self) -> f64 {
        let mut delay = self.initial_delay;
        let mut remaining = self.attempt;
        while remaining > 0 && delay < self.maximum_delay {
            delay *= self.multiplier;
            remaining -= 1;
        }
        delay.min(self.maximum_delay)
    }

    /// The delay to wait; advances to the next attempt. `random` returns values in 0..1.
    pub fn next_delay(&mut self, random: impl FnOnce() -> f64) -> f64 {
        let base = self.base_delay();
        self.attempt += 1;
        if self.jitter <= 0.0 {
            return base;
        }
        let factor = 1.0 + self.jitter * (random() * 2.0 - 1.0);
        (base * factor).clamp(0.0, self.maximum_delay)
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// A cheap pseudo-random number in 0..1 for jitter (no `rand` crate).
pub fn jitter_random() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    (hasher.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// Fixed-capacity FIFO that overwrites the oldest element, so history uses constant memory.
#[derive(Debug, Clone)]
pub struct RingBuffer<T> {
    capacity: usize,
    storage: Vec<Option<T>>,
    head: usize,
    count: usize,
}

impl<T: Clone> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "RingBuffer capacity must be positive");
        RingBuffer {
            capacity,
            storage: vec![None; capacity],
            head: 0,
            count: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn is_full(&self) -> bool {
        self.count == self.capacity
    }

    pub fn push(&mut self, element: T) {
        let index = (self.head + self.count) % self.capacity;
        self.storage[index] = Some(element);
        if self.count < self.capacity {
            self.count += 1;
        } else {
            self.head = (self.head + 1) % self.capacity;
        }
    }

    pub fn clear(&mut self) {
        self.storage = vec![None; self.capacity];
        self.head = 0;
        self.count = 0;
    }

    /// Oldest-first access.
    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.count {
            return None;
        }
        self.storage[(self.head + index) % self.capacity].as_ref()
    }

    pub fn last(&self) -> Option<&T> {
        self.count.checked_sub(1).and_then(|i| self.get(i))
    }

    pub fn first(&self) -> Option<&T> {
        self.get(0)
    }

    /// Oldest-first copy.
    pub fn elements(&self) -> Vec<T> {
        (0..self.count)
            .filter_map(|i| self.get(i).cloned())
            .collect()
    }

    /// The newest `n` elements, oldest first.
    pub fn suffix(&self, n: usize) -> Vec<T> {
        let take = n.min(self.count);
        (self.count - take..self.count)
            .filter_map(|i| self.get(i).cloned())
            .collect()
    }
}

/// One computed resource reading for a container (or the host aggregate).
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceSample {
    pub timestamp: f64,
    /// Percent of a single CPU (4 busy cores = 400%), like `docker stats`.
    pub cpu_percent: f64,
    pub cpu_count: i64,
    pub memory_usage: u64,
    pub memory_limit: u64,
    pub network_rx_rate: f64,
    pub network_tx_rate: f64,
    pub block_read_rate: f64,
    pub block_write_rate: f64,
    pub network_rx_total: u64,
    pub network_tx_total: u64,
    pub block_read_total: u64,
    pub block_write_total: u64,
    pub pids: u64,
    /// Share of the host's GPUs this container keeps busy (0-100), None when unknown.
    pub gpu_percent: Option<f64>,
    /// GPU memory used by the container's processes, None when unknown.
    pub gpu_memory: Option<u64>,
}

impl Default for ResourceSample {
    fn default() -> Self {
        ResourceSample {
            timestamp: 0.0,
            cpu_percent: 0.0,
            cpu_count: 1,
            memory_usage: 0,
            memory_limit: 0,
            network_rx_rate: 0.0,
            network_tx_rate: 0.0,
            block_read_rate: 0.0,
            block_write_rate: 0.0,
            network_rx_total: 0,
            network_tx_total: 0,
            block_read_total: 0,
            block_write_total: 0,
            pids: 0,
            gpu_percent: None,
            gpu_memory: None,
        }
    }
}

impl ResourceSample {
    pub fn memory_percent(&self) -> f64 {
        if self.memory_limit == 0 {
            return 0.0;
        }
        self.memory_usage as f64 / self.memory_limit as f64 * 100.0
    }

    /// CPU normalized to the whole machine (0-100%), which is what graphs plot.
    pub fn normalized_cpu_percent(&self) -> f64 {
        self.cpu_percent / self.cpu_count.max(1) as f64
    }
}

/// Bytes per second; counter resets (container restarts) give 0 instead of a spike.
pub fn rate(current: u64, previous: u64, elapsed_seconds: f64) -> f64 {
    if elapsed_seconds <= 0.0 || current < previous {
        return 0.0;
    }
    (current - previous) as f64 / elapsed_seconds
}

/// Linux: share of host CPU time scaled by CPU count. Windows (no system usage): 100ns ticks over
/// wall-clock time.
pub fn cpu_percent(current: &CpuStats, baseline: &CpuStats, elapsed_seconds: Option<f64>) -> f64 {
    if current.usage.total_usage < baseline.usage.total_usage {
        return 0.0;
    }
    let cpu_delta = (current.usage.total_usage - baseline.usage.total_usage) as f64;
    if let (Some(system), Some(previous_system)) = (current.system_usage, baseline.system_usage) {
        if previous_system > 0 {
            if system <= previous_system {
                return 0.0;
            }
            let value = cpu_delta / (system - previous_system) as f64
                * current.effective_cpu_count() as f64
                * 100.0;
            return if value.is_finite() {
                value.max(0.0)
            } else {
                0.0
            };
        }
    }
    if current.system_usage.is_none() && baseline.usage.total_usage > 0 {
        if let Some(elapsed) = elapsed_seconds.filter(|e| *e > 0.0) {
            // Windows counts 100ns ticks; dtop keeps its single-CPU convention (400% = 4 cores).
            let value = cpu_delta / (elapsed * 10_000_000.0) * 100.0;
            return if value.is_finite() {
                value.max(0.0)
            } else {
                0.0
            };
        }
    }
    0.0
}

/// A sample from the current payload and, when available, the previous one. With one-shot
/// stats Docker leaves `precpu_stats` empty, so our previous sample is the baseline; otherwise
/// `precpu_stats` is the fallback.
pub fn sample(
    current: &ContainerStats,
    previous: Option<&ContainerStats>,
    fallback_timestamp: f64,
) -> ResourceSample {
    let ts = timestamp::parse(&current.read).unwrap_or(fallback_timestamp);
    let (rx, tx) = current.total_network();
    let (read, write) = current.total_block_io();
    let mut s = ResourceSample {
        timestamp: ts,
        cpu_count: current.cpu.effective_cpu_count(),
        memory_usage: current.memory_usage_excluding_cache(),
        memory_limit: current.memory.limit.unwrap_or(0),
        network_rx_total: rx,
        network_tx_total: tx,
        block_read_total: read,
        block_write_total: write,
        pids: current
            .pids_current
            .unwrap_or(current.num_procs.max(0) as u64),
        ..Default::default()
    };
    if current.num_procs > 0 && current.cpu.system_usage.is_none() {
        // Windows reports processors through num_procs.
        s.cpu_count = current.num_procs;
    }
    let (baseline_cpu, baseline_ts) = match previous {
        Some(p) => (&p.cpu, timestamp::parse(&p.read)),
        None => (&current.precpu, timestamp::parse(&current.preread)),
    };
    s.cpu_percent = cpu_percent(&current.cpu, baseline_cpu, baseline_ts.map(|b| ts - b));
    if let (Some(previous), Some(baseline_ts)) = (previous, baseline_ts) {
        let elapsed = ts - baseline_ts;
        if elapsed > 0.0 {
            let (prx, ptx) = previous.total_network();
            let (pread, pwrite) = previous.total_block_io();
            s.network_rx_rate = rate(rx, prx, elapsed);
            s.network_tx_rate = rate(tx, ptx, elapsed);
            s.block_read_rate = rate(read, pread, elapsed);
            s.block_write_rate = rate(write, pwrite, elapsed);
        }
    }
    s
}

/// Keeps the previous raw payload per container so each new one can become rates.
#[derive(Debug, Clone, Default)]
pub struct StatsSampler {
    previous: HashMap<String, ContainerStats>,
}

impl StatsSampler {
    pub fn ingest(&mut self, id: &str, stats: ContainerStats, now: f64) -> ResourceSample {
        let result = sample(&stats, self.previous.get(id), now);
        self.previous.insert(id.to_string(), stats);
        result
    }

    /// Drops baselines for containers that are gone or stopped.
    pub fn retain(&mut self, ids: &HashSet<String>) {
        self.previous.retain(|k, _| ids.contains(k));
    }

    pub fn tracked_ids(&self) -> HashSet<String> {
        self.previous.keys().cloned().collect()
    }
}

/// Rolling series for one container (or the host).
#[derive(Debug, Clone)]
pub struct MetricSeries {
    pub cpu: RingBuffer<f64>,
    pub memory: RingBuffer<f64>,
    pub network_rx: RingBuffer<f64>,
    pub network_tx: RingBuffer<f64>,
    pub block_read: RingBuffer<f64>,
    pub block_write: RingBuffer<f64>,
    /// GPU utilization (0-100): per container its share, for the host the device average.
    pub gpu: RingBuffer<f64>,
}

impl MetricSeries {
    pub fn new(capacity: usize) -> Self {
        MetricSeries {
            cpu: RingBuffer::new(capacity),
            memory: RingBuffer::new(capacity),
            network_rx: RingBuffer::new(capacity),
            network_tx: RingBuffer::new(capacity),
            block_read: RingBuffer::new(capacity),
            block_write: RingBuffer::new(capacity),
            gpu: RingBuffer::new(capacity),
        }
    }

    pub fn push(&mut self, s: &ResourceSample) {
        self.cpu.push(s.cpu_percent);
        self.memory.push(s.memory_usage as f64);
        self.network_rx.push(s.network_rx_rate);
        self.network_tx.push(s.network_tx_rate);
        self.block_read.push(s.block_read_rate);
        self.block_write.push(s.block_write_rate);
        self.gpu.push(s.gpu_percent.unwrap_or(0.0));
    }
}

/// Latest samples and history for every container plus the host aggregate.
#[derive(Debug, Clone)]
pub struct MetricsStore {
    pub capacity: usize,
    pub latest: BTreeMap<String, ResourceSample>,
    pub series: BTreeMap<String, MetricSeries>,
    pub total: MetricSeries,
    pub total_latest: ResourceSample,
    /// Latest host GPU readings (empty without a GPU or with collection off).
    pub gpu: GpuSample,
}

impl Default for MetricsStore {
    fn default() -> Self {
        MetricsStore::new(240)
    }
}

impl MetricsStore {
    pub fn new(capacity: usize) -> Self {
        MetricsStore {
            capacity,
            latest: BTreeMap::new(),
            series: BTreeMap::new(),
            total: MetricSeries::new(capacity),
            total_latest: ResourceSample::default(),
            gpu: GpuSample::default(),
        }
    }

    /// Records one polling round. Containers missing from `samples` lose their history, so the
    /// store never grows with churned containers. The aggregate's GPU fields come from the host
    /// devices rather than from summing containers.
    pub fn record(
        &mut self,
        samples: BTreeMap<String, ResourceSample>,
        timestamp: f64,
        gpu: Option<GpuSample>,
    ) {
        self.series.retain(|id, _| samples.contains_key(id));
        for (id, s) in &samples {
            self.series
                .entry(id.clone())
                .or_insert_with(|| MetricSeries::new(self.capacity))
                .push(s);
        }
        let mut aggregate = ResourceSample {
            timestamp,
            cpu_count: samples.values().map(|s| s.cpu_count).max().unwrap_or(1),
            ..Default::default()
        };
        for s in samples.values() {
            aggregate.cpu_percent += s.cpu_percent;
            aggregate.memory_usage = aggregate.memory_usage.wrapping_add(s.memory_usage);
            aggregate.network_rx_rate += s.network_rx_rate;
            aggregate.network_tx_rate += s.network_tx_rate;
            aggregate.block_read_rate += s.block_read_rate;
            aggregate.block_write_rate += s.block_write_rate;
            aggregate.pids = aggregate.pids.wrapping_add(s.pids);
        }
        if let Some(gpu) = gpu.as_ref().filter(|g| !g.devices.is_empty()) {
            aggregate.gpu_percent = gpu.average_utilization();
            aggregate.gpu_memory = gpu.total_memory_used();
        }
        self.gpu = gpu.unwrap_or_default();
        self.latest = samples;
        self.total.push(&aggregate);
        self.total_latest = aggregate;
    }

    pub fn reset(&mut self) {
        *self = MetricsStore::new(self.capacity);
    }
}

/// A process inside a container, from `docker top`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerProcess {
    pub pid: i64,
    pub parent_pid: Option<i64>,
    pub user: String,
    pub cpu: String,
    pub command: String,
}

/// One flattened row of a rendered tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTreeRow {
    pub process: ContainerProcess,
    pub depth: usize,
    /// For each ancestor level, whether a vertical guide continues below this row.
    pub guides: Vec<bool>,
    pub is_last: bool,
}

impl ProcessTreeRow {
    /// Box-drawing prefix such as `│  ├─ `.
    pub fn prefix(&self) -> String {
        if self.depth == 0 {
            return String::new();
        }
        let mut text: String = self
            .guides
            .iter()
            .skip(1)
            .map(|&c| if c { "│  " } else { "   " })
            .collect();
        text.push_str(if self.is_last { "└─ " } else { "├─ " });
        text
    }
}

/// Parses `docker top` by column title, so `ps -ef` output (Linux) and Windows' reduced columns
/// both work.
pub fn parse_processes(top: &ContainerTop) -> Vec<ContainerProcess> {
    let titles: Vec<String> = top.titles.iter().map(|t| t.to_uppercase()).collect();
    let column = |names: &[&str]| titles.iter().position(|t| names.contains(&t.as_str()));
    let Some(pid_column) = column(&["PID"]) else {
        return Vec::new();
    };
    let ppid_column = column(&["PPID"]);
    let user_column = column(&["UID", "USER"]);
    let cpu_column = column(&["C", "%CPU", "CPU"]);
    let command_column = column(&["CMD", "COMMAND", "NAME", "ARGS"]);
    top.processes
        .iter()
        .filter_map(|row| {
            let pid: i64 = row.get(pid_column)?.trim().parse().ok()?;
            let value =
                |index: Option<usize>| index.and_then(|i| row.get(i)).cloned().unwrap_or_default();
            let command = match command_column {
                // `ps` may split a command with spaces across extra trailing cells, but only when
                // the command is the last column (Windows lists the image name first).
                Some(c) if c < row.len() => {
                    if c == titles.len() - 1 {
                        row[c..].join(" ")
                    } else {
                        row[c].clone()
                    }
                }
                _ => row.last().cloned().unwrap_or_default(),
            };
            Some(ContainerProcess {
                pid,
                parent_pid: ppid_column
                    .and_then(|c| row.get(c))
                    .and_then(|v| v.trim().parse().ok()),
                user: value(user_column),
                cpu: value(cpu_column),
                command,
            })
        })
        .collect()
}

/// A depth-first flattened tree. Processes whose parent is absent (usually the container's
/// init) become roots. Cycles are broken defensively.
pub fn build_process_tree(processes: &[ContainerProcess]) -> Vec<ProcessTreeRow> {
    let pids: HashSet<i64> = processes.iter().map(|p| p.pid).collect();
    let mut children: HashMap<i64, Vec<&ContainerProcess>> = HashMap::new();
    let mut roots: Vec<&ContainerProcess> = Vec::new();
    for p in processes {
        match p.parent_pid {
            Some(parent) if parent != p.pid && pids.contains(&parent) => {
                children.entry(parent).or_default().push(p)
            }
            _ => roots.push(p),
        }
    }
    for kids in children.values_mut() {
        kids.sort_by_key(|p| p.pid);
    }
    roots.sort_by_key(|p| p.pid);
    let mut rows = Vec::new();
    let mut visited = HashSet::new();
    fn visit(
        p: &ContainerProcess,
        depth: usize,
        guides: Vec<bool>,
        is_last: bool,
        children: &HashMap<i64, Vec<&ContainerProcess>>,
        visited: &mut HashSet<i64>,
        rows: &mut Vec<ProcessTreeRow>,
    ) {
        if !visited.insert(p.pid) {
            return;
        }
        rows.push(ProcessTreeRow {
            process: p.clone(),
            depth,
            guides: guides.clone(),
            is_last,
        });
        let kids = children.get(&p.pid).cloned().unwrap_or_default();
        for (i, child) in kids.iter().enumerate() {
            let mut next = guides.clone();
            next.push(!is_last);
            visit(
                child,
                depth + 1,
                next,
                i == kids.len() - 1,
                children,
                visited,
                rows,
            );
        }
    }
    for (i, root) in roots.iter().enumerate() {
        visit(
            root,
            0,
            Vec::new(),
            i == roots.len() - 1,
            &children,
            &mut visited,
            &mut rows,
        );
    }
    // Anything reachable only through a cycle is appended flat so no process disappears.
    for p in processes {
        if !visited.contains(&p.pid) {
            visit(p, 0, Vec::new(), true, &children, &mut visited, &mut rows);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse_str;

    #[test]
    fn backoff_grows_and_caps() {
        let mut b = Backoff::new(0.5, 4.0, 2.0, 0.0);
        let delays: Vec<f64> = (0..6).map(|_| b.next_delay(|| 0.5)).collect();
        assert_eq!(delays, vec![0.5, 1.0, 2.0, 4.0, 4.0, 4.0]);
        b.reset();
        assert_eq!(b.base_delay(), 0.5);
        let mut j = Backoff::new(1.0, 10.0, 2.0, 0.2);
        assert!((j.next_delay(|| 1.0) - 1.2).abs() < 1e-9);
    }

    #[test]
    fn ring_buffer_overwrites_oldest() {
        let mut r = RingBuffer::new(3);
        for v in 1..=5 {
            r.push(v);
        }
        assert_eq!(r.elements(), vec![3, 4, 5]);
        assert_eq!(r.suffix(2), vec![4, 5]);
        assert_eq!(r.last(), Some(&5));
        assert!(r.is_full());
    }

    fn stats(json: &str) -> ContainerStats {
        ContainerStats::decode(&parse_str(json).unwrap())
    }

    #[test]
    fn linux_cpu_and_rates_from_previous_sample() {
        let a = stats(
            r#"{"read":"2024-01-01T00:00:00Z","cpu_stats":{"cpu_usage":{"total_usage":1000},"system_cpu_usage":10000,"online_cpus":4},
                "networks":{"eth0":{"rx_bytes":100,"tx_bytes":0}}}"#,
        );
        let b = stats(
            r#"{"read":"2024-01-01T00:00:02Z","cpu_stats":{"cpu_usage":{"total_usage":2000},"system_cpu_usage":20000,"online_cpus":4},
                "networks":{"eth0":{"rx_bytes":300,"tx_bytes":0}}}"#,
        );
        let s = sample(&b, Some(&a), 0.0);
        assert!((s.cpu_percent - 40.0).abs() < 1e-9);
        assert!((s.network_rx_rate - 100.0).abs() < 1e-9);
        assert!((s.normalized_cpu_percent() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn windows_cpu_uses_ticks() {
        let a = stats(
            r#"{"read":"2024-01-01T00:00:00Z","num_procs":2,"cpu_stats":{"cpu_usage":{"total_usage":10000000}}}"#,
        );
        let b = stats(
            r#"{"read":"2024-01-01T00:00:01Z","num_procs":2,"cpu_stats":{"cpu_usage":{"total_usage":15000000}}}"#,
        );
        let s = sample(&b, Some(&a), 0.0);
        assert!((s.cpu_percent - 50.0).abs() < 1e-9);
        assert_eq!(s.cpu_count, 2);
    }

    #[test]
    fn counter_reset_gives_zero_rate() {
        assert_eq!(rate(5, 10, 1.0), 0.0);
    }

    #[test]
    fn metrics_store_drops_churned_containers() {
        let mut store = MetricsStore::new(10);
        let mut samples = BTreeMap::new();
        samples.insert(
            "a".to_string(),
            ResourceSample {
                cpu_percent: 10.0,
                ..Default::default()
            },
        );
        samples.insert(
            "b".to_string(),
            ResourceSample {
                cpu_percent: 5.0,
                ..Default::default()
            },
        );
        store.record(samples, 1.0, None);
        assert!((store.total_latest.cpu_percent - 15.0).abs() < 1e-9);
        let mut samples = BTreeMap::new();
        samples.insert("a".to_string(), ResourceSample::default());
        store.record(samples, 2.0, None);
        assert!(store.series.contains_key("a") && !store.series.contains_key("b"));
        assert_eq!(store.total.cpu.len(), 2);
    }

    #[test]
    fn builds_process_trees_with_guides() {
        let top = ContainerTop {
            titles: vec![
                "UID".into(),
                "PID".into(),
                "PPID".into(),
                "C".into(),
                "CMD".into(),
            ],
            processes: vec![
                vec![
                    "root".into(),
                    "1".into(),
                    "0".into(),
                    "0".into(),
                    "init".into(),
                ],
                vec![
                    "root".into(),
                    "7".into(),
                    "1".into(),
                    "0".into(),
                    "worker".into(),
                    "--x".into(),
                ],
                vec![
                    "root".into(),
                    "9".into(),
                    "7".into(),
                    "0".into(),
                    "child".into(),
                ],
                vec![
                    "root".into(),
                    "8".into(),
                    "1".into(),
                    "0".into(),
                    "other".into(),
                ],
            ],
        };
        let processes = parse_processes(&top);
        assert_eq!(processes[1].command, "worker --x");
        let rows = build_process_tree(&processes);
        let rendered: Vec<String> = rows
            .iter()
            .map(|r| format!("{}{}", r.prefix(), r.process.pid))
            .collect();
        assert_eq!(rendered, vec!["1", "├─ 7", "│  └─ 9", "└─ 8"]);
    }

    #[test]
    fn cycles_do_not_lose_processes() {
        let p = |pid, ppid| ContainerProcess {
            pid,
            parent_pid: Some(ppid),
            user: String::new(),
            cpu: String::new(),
            command: String::new(),
        };
        let rows = build_process_tree(&[p(2, 3), p(3, 2)]);
        assert_eq!(rows.len(), 2);
    }
}
