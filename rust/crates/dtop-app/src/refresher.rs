//! Polls the Docker API and turns responses into snapshots. State lives behind a mutex that is
//! held only while it changes, never across network calls, so refreshes, detail loads and
//! operations from different threads interleave like the Swift actor's awaits did.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use dtop_core::compose::{
    compose_invocation, CommandRunning, ComposeOperation, ComposeProject, ProcessCommandRunner,
};
use dtop_core::gpu::{GpuCollecting, GpuSample};
use dtop_core::host::DockerHost;
use dtop_core::models::{ContainerState, ContainerStats};
use dtop_core::monitoring::{
    build_process_tree, jitter_random, parse_processes, Backoff, MetricsStore, ResourceSample,
    StatsSampler,
};
use dtop_core::{format, DockerApi, DockerError};

use crate::operation::{DockerOperation, OperationResult, PruneTarget};
use crate::state::{ConnectionState, ContainerDetailData, DetailPane, DockerSnapshot};

pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

struct Inner {
    snapshot: DockerSnapshot,
    sampler: StatsSampler,
    backoff: Backoff,
}

pub struct DataRefresher {
    pub api: Arc<dyn DockerApi>,
    pub command_runner: Arc<dyn CommandRunning>,
    pub gpu_collector: Option<Arc<dyn GpuCollecting>>,
    /// The host, when compose commands must name it (None: the docker CLI's default).
    pub host: Option<DockerHost>,
    pub stats_concurrency: usize,
    pub log_tail: usize,
    clock: Clock,
    random: Arc<dyn Fn() -> f64 + Send + Sync>,
    inner: Mutex<Inner>,
}

pub struct RefresherOptions {
    pub host: Option<DockerHost>,
    pub command_runner: Arc<dyn CommandRunning>,
    pub gpu_collector: Option<Arc<dyn GpuCollecting>>,
    pub history_size: usize,
    pub stats_concurrency: usize,
    pub log_tail: usize,
    pub backoff: Backoff,
    pub clock: Clock,
    pub random: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl Default for RefresherOptions {
    fn default() -> Self {
        RefresherOptions {
            host: None,
            command_runner: Arc::new(ProcessCommandRunner::default()),
            gpu_collector: None,
            history_size: 240,
            stats_concurrency: 8,
            log_tail: 300,
            backoff: Backoff::default(),
            clock: Arc::new(format::now),
            random: Arc::new(jitter_random),
        }
    }
}

pub fn describe(error: &DockerError) -> String {
    error.to_string()
}

impl DataRefresher {
    pub fn new(
        api: Arc<dyn DockerApi>,
        endpoint_description: &str,
        options: RefresherOptions,
    ) -> Self {
        let snapshot = DockerSnapshot {
            endpoint: endpoint_description.into(),
            metrics: MetricsStore::new(options.history_size),
            ..DockerSnapshot::default()
        };
        DataRefresher {
            api,
            command_runner: options.command_runner,
            gpu_collector: options.gpu_collector,
            host: options.host,
            stats_concurrency: options.stats_concurrency.max(1),
            log_tail: options.log_tail,
            clock: options.clock,
            random: options.random,
            inner: Mutex::new(Inner {
                snapshot,
                sampler: StatsSampler::default(),
                backoff: options.backoff,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn snapshot(&self) -> DockerSnapshot {
        self.lock().snapshot.clone()
    }

    /// One polling round: containers and stats every time; images, networks and volumes on
    /// request. Failures mark the connection down with a backoff-scheduled retry time.
    pub fn refresh(&self, include_inventory: bool) -> DockerSnapshot {
        let now = (self.clock)();
        match self.poll(include_inventory, now) {
            Ok(()) => {}
            Err(error) => self.mark_disconnected(&error, now),
        }
        self.snapshot()
    }

    fn poll(&self, include_inventory: bool, now: f64) -> Result<(), DockerError> {
        let needs_version = {
            let inner = self.lock();
            inner.snapshot.version.is_none() || !inner.snapshot.connection.is_connected()
        };
        if needs_version {
            let version = self.api.version()?;
            self.lock().snapshot.version = Some(version);
        }
        let containers = self.api.list_containers(true)?;
        let active: Vec<String> = containers
            .iter()
            .filter(|c| {
                matches!(
                    c.state,
                    ContainerState::Running | ContainerState::Restarting
                )
            })
            .map(|c| c.id.clone())
            .collect();
        let raw = self.fetch_stats(&active)?;
        let gpu = self.gpu_collector.as_ref().map(|g| g.sample(now));
        let needs_info = include_inventory || self.lock().snapshot.info.is_none();
        let mut warnings = Vec::new();
        let info = if needs_info {
            match self.api.info() {
                Ok(info) => Some(info),
                Err(e) => {
                    warnings.push(format!("info: {}", describe(&e)));
                    None
                }
            }
        } else {
            None
        };
        let inventory = include_inventory.then(|| {
            let images = self
                .api
                .list_images()
                .map_err(|e| warnings.push(format!("images: {}", describe(&e))))
                .ok();
            let networks = self
                .api
                .list_networks()
                .map_err(|e| warnings.push(format!("networks: {}", describe(&e))))
                .ok();
            let volumes = self
                .api
                .list_volumes()
                .map_err(|e| warnings.push(format!("volumes: {}", describe(&e))))
                .ok();
            (images, networks, volumes)
        });
        let mut inner = self.lock();
        let mut samples: BTreeMap<String, ResourceSample> = BTreeMap::new();
        for (id, stats) in &raw {
            samples.insert(id.clone(), inner.sampler.ingest(id, stats.clone(), now));
        }
        let ids: HashSet<String> = raw.iter().map(|(id, _)| id.clone()).collect();
        inner.sampler.retain(&ids);
        if let Some(gpu) = gpu.as_ref().filter(|g| !g.devices.is_empty()) {
            attribute_gpu(gpu, &mut samples);
        }
        inner.snapshot.metrics.record(samples, now, gpu);
        inner.snapshot.containers = containers;
        if let Some(info) = info {
            inner.snapshot.info = Some(info);
        }
        if let Some((images, networks, volumes)) = inventory {
            if let Some(images) = images {
                inner.snapshot.images = images;
            }
            if let Some(networks) = networks {
                inner.snapshot.networks = networks;
            }
            if let Some(volumes) = volumes {
                inner.snapshot.volumes = volumes;
            }
            inner.snapshot.warnings = warnings;
        }
        inner.snapshot.connection = ConnectionState::Connected;
        inner.snapshot.last_updated = Some(now);
        inner.backoff.reset();
        Ok(())
    }

    fn mark_disconnected(&self, error: &DockerError, now: f64) {
        let random = Arc::clone(&self.random);
        let mut inner = self.lock();
        let delay = inner.backoff.next_delay(|| random());
        inner.snapshot.connection = ConnectionState::Disconnected {
            message: describe(error),
            retry_at: Some(now + delay),
        };
        // Stale samples would draw misleading flat lines; drop rates but keep the lists.
        inner.sampler = StatsSampler::default();
        inner.snapshot.version = None;
    }

    /// Seconds until the next reconnect attempt is due (0 when connected).
    pub fn retry_delay(&self) -> f64 {
        match &self.lock().snapshot.connection {
            ConnectionState::Disconnected {
                retry_at: Some(at), ..
            } => (at - (self.clock)()).max(0.0),
            _ => 0.0,
        }
    }

    /// Stats with bounded concurrency; individual failures (a container that just stopped) are
    /// skipped rather than failing the round.
    pub fn fetch_stats(
        &self,
        ids: &[String],
    ) -> Result<Vec<(String, ContainerStats)>, DockerError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let next = AtomicUsize::new(0);
        let results = Mutex::new(Vec::new());
        let workers = self.stats_concurrency.min(ids.len());
        std::thread::scope(|scope| -> Result<(), DockerError> {
            let mut handles = Vec::with_capacity(workers);
            let mut failure = None;
            for _ in 0..workers {
                match std::thread::Builder::new()
                    .name("dtop-stats".into())
                    .spawn_scoped(scope, || -> Result<(), DockerError> {
                        loop {
                            let index = next.fetch_add(1, Ordering::SeqCst);
                            let Some(id) = ids.get(index) else { break };
                            if let Ok(stats) = self.api.stats(id) {
                                results
                                    .lock()
                                    .map_err(|_| {
                                        DockerError::Io("stats result lock was poisoned".into())
                                    })?
                                    .push((id.clone(), stats));
                            }
                        }
                        Ok(())
                    }) {
                    Ok(handle) => handles.push(handle),
                    Err(error) => {
                        failure = Some(DockerError::Io(format!(
                            "could not start a stats worker: {error} — check system thread limits"
                        )));
                        break;
                    }
                }
            }
            for handle in handles {
                match handle.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        failure.get_or_insert(error);
                    }
                    Err(_) => {
                        failure.get_or_insert_with(|| {
                            DockerError::Io("a stats worker failed unexpectedly".into())
                        });
                    }
                }
            }
            failure.map_or(Ok(()), Err)
        })?;
        results
            .into_inner()
            .map_err(|_| DockerError::Io("stats result lock was poisoned".into()))
    }

    /// Loads detail-pane data for one container.
    pub fn load_detail(
        &self,
        container_id: &str,
        pane: DetailPane,
        is_active: bool,
    ) -> ContainerDetailData {
        let mut data = {
            let inner = self.lock();
            inner
                .snapshot
                .detail
                .clone()
                .filter(|d| d.container_id == container_id)
                .unwrap_or_else(|| ContainerDetailData::new(container_id))
        };
        data.error = None;
        let result: Result<(), DockerError> = (|| {
            match pane {
                DetailPane::Overview => {
                    data.details = Some(self.api.inspect_container(container_id)?)
                }
                DetailPane::Processes => {
                    data.processes = if is_active {
                        build_process_tree(&parse_processes(&self.api.top(container_id)?))
                    } else {
                        Vec::new()
                    };
                }
                DetailPane::Logs => data.logs = self.api.logs(container_id, self.log_tail)?,
            }
            Ok(())
        })();
        if let Err(error) = result {
            data.error = Some(describe(&error));
        }
        data.loaded_at = (self.clock)();
        self.lock().snapshot.detail = Some(data.clone());
        data
    }

    pub fn clear_detail(&self) {
        self.lock().snapshot.detail = None;
    }

    /// Executes a management operation and reports a human-readable outcome.
    pub fn perform(&self, operation: &DockerOperation) -> OperationResult {
        match self.execute(operation) {
            Ok(message) => OperationResult {
                operation: operation.clone(),
                succeeded: true,
                message,
            },
            Err(error) => OperationResult {
                operation: operation.clone(),
                succeeded: false,
                message: describe(&error),
            },
        }
    }

    fn execute(&self, operation: &DockerOperation) -> Result<String, DockerError> {
        match operation {
            DockerOperation::Container { action, id, .. } => self.api.perform(action, id)?,
            DockerOperation::RemoveContainer { id, force, .. } => {
                self.api.remove_container(id, *force, false)?;
                let mut inner = self.lock();
                if inner
                    .snapshot
                    .detail
                    .as_ref()
                    .is_some_and(|d| &d.container_id == id)
                {
                    inner.snapshot.detail = None;
                }
            }
            DockerOperation::Compose {
                operation: compose,
                project,
            } => return self.execute_compose(*compose, project),
            DockerOperation::RemoveImage { id, force, .. } => self.api.remove_image(id, *force)?,
            DockerOperation::RemoveNetwork { id, .. } => self.api.remove_network(id)?,
            DockerOperation::RemoveVolume { name } => self.api.remove_volume(name, false)?,
            DockerOperation::Prune(target) => {
                let report = match target {
                    PruneTarget::Containers => self.api.prune_containers()?,
                    PruneTarget::Images => self.api.prune_images(true)?,
                    PruneTarget::Networks => self.api.prune_networks()?,
                    PruneTarget::Volumes => self.api.prune_volumes()?,
                };
                return Ok(format!(
                    "{}: {} removed, {} reclaimed",
                    operation.success_description(),
                    report.deleted.len(),
                    format::bytes(report.space_reclaimed as f64)
                ));
            }
        }
        Ok(operation.success_description())
    }

    fn execute_compose(
        &self,
        operation: ComposeOperation,
        project_name: &str,
    ) -> Result<String, DockerError> {
        let containers = self.api.list_containers(true)?;
        let project = ComposeProject::group(&containers)
            .0
            .into_iter()
            .find(|p| p.name == project_name)
            .ok_or_else(|| DockerError::Http {
                status: 404,
                message: format!("compose project '{project_name}' has no containers"),
            })?;
        let verb = operation.raw_value();
        if let (true, Some(action)) = (
            operation.is_engine_native(),
            operation.container_action(None),
        ) {
            let targets: Vec<_> = project
                .containers()
                .filter(|c| operation.applies_to(c))
                .collect();
            if targets.is_empty() {
                return Ok(format!("compose {verb} {project_name}: nothing to do"));
            }
            let failures: Vec<String> = targets
                .iter()
                .filter_map(|c| {
                    self.api
                        .perform(&action, &c.id)
                        .err()
                        .map(|e| format!("{}: {}", c.name(), describe(&e)))
                })
                .collect();
            if !failures.is_empty() {
                return Err(DockerError::Io(format!(
                    "compose {verb} failed for {}",
                    failures.join("; ")
                )));
            }
            return Ok(format!(
                "compose {verb} {project_name}: {} container(s)",
                targets.len()
            ));
        }
        let invocation =
            compose_invocation(operation, &project, "docker", None, self.host.as_ref());
        let result = self.command_runner.run(&invocation)?;
        if !result.succeeded() {
            let last = result
                .output
                .lines()
                .last()
                .map_or_else(|| format!("exit code {}", result.exit_code), str::to_string);
            return Err(DockerError::Io(format!(
                "{} failed: {last}",
                invocation.display_string()
            )));
        }
        Ok(format!("compose {verb} {project_name} finished"))
    }
}

/// Adds per-container GPU usage to samples. Containers in the GPU process list get their share;
/// with complete attribution the rest are known to be at 0, otherwise they stay unknown.
pub fn attribute_gpu(gpu: &GpuSample, samples: &mut BTreeMap<String, ResourceSample>) {
    let usage = gpu.usage_by_container();
    for (id, sample) in samples.iter_mut() {
        if let Some((utilization, memory)) = usage.get(id) {
            sample.gpu_percent = Some(*utilization);
            sample.gpu_memory = Some(*memory);
        } else if gpu.process_attribution_complete {
            sample.gpu_percent = Some(0.0);
            sample.gpu_memory = Some(0);
        }
    }
}
