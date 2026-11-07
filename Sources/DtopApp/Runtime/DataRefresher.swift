import DtopCore

/// Polls the Docker API and turns responses into snapshots. An actor, so refreshes, detail loads
/// and operations triggered from different tasks never race on the sampler or history.
public actor DataRefresher {
    public let api: any DockerAPI
    public let commandRunner: any CommandRunning
    public let gpuCollector: (any GPUCollecting)?
    public let endpoint: DockerEndpoint?
    public private(set) var snapshot: DockerSnapshot
    private var sampler = StatsSampler()
    private var backoff: Backoff
    private let clock: @Sendable () -> Double
    private let random: @Sendable () -> Double
    public let statsConcurrency: Int
    public let logTail: Int

    public init(
        api: any DockerAPI,
        endpointDescription: String,
        endpoint: DockerEndpoint? = nil,
        commandRunner: any CommandRunning = ProcessCommandRunner(),
        gpuCollector: (any GPUCollecting)? = nil,
        historySize: Int = 240,
        statsConcurrency: Int = 8,
        logTail: Int = 300,
        backoff: Backoff = Backoff(),
        clock: @escaping @Sendable () -> Double,
        random: @escaping @Sendable () -> Double = { Double.random(in: 0..<1) }
    ) {
        self.api = api
        self.endpoint = endpoint
        self.commandRunner = commandRunner
        self.gpuCollector = gpuCollector
        self.snapshot = DockerSnapshot(endpoint: endpointDescription, metrics: MetricsStore(capacity: historySize))
        self.statsConcurrency = max(1, statsConcurrency)
        self.logTail = logTail
        self.backoff = backoff
        self.clock = clock
        self.random = random
    }

    /// One polling round. Containers and stats every time; images/networks/volumes on request.
    /// Failures mark the connection down with a backoff-scheduled retry time instead of throwing.
    @discardableResult
    public func refresh(includeInventory: Bool) async -> DockerSnapshot {
        let now = clock()
        do {
            if snapshot.version == nil || !snapshot.connection.isConnected {
                snapshot.version = try await api.version()
            }
            let containers = try await api.listContainers(all: true)
            let active = containers.filter { $0.state == .running || $0.state == .restarting }
            let rawStats = await fetchStats(ids: active.map(\.id))

            var samples: [String: ResourceSample] = [:]
            for (id, stats) in rawStats {
                samples[id] = sampler.ingest(id: id, stats: stats, now: now)
            }
            sampler.retain(ids: Set(rawStats.keys))
            let gpu = await gpuCollector?.sample(now: now)
            if let gpu, !gpu.devices.isEmpty {
                Self.attributeGPU(gpu, to: &samples)
            }
            snapshot.metrics.record(samples, timestamp: now, gpu: gpu)
            snapshot.containers = containers

            var warnings: [String] = []
            if includeInventory || snapshot.info == nil {
                do { snapshot.info = try await api.info() } catch { warnings.append("info: \(Self.describe(error))") }
            }
            if includeInventory {
                do { snapshot.images = try await api.listImages() } catch { warnings.append("images: \(Self.describe(error))") }
                do { snapshot.networks = try await api.listNetworks() } catch { warnings.append("networks: \(Self.describe(error))") }
                do { snapshot.volumes = try await api.listVolumes() } catch { warnings.append("volumes: \(Self.describe(error))") }
                snapshot.warnings = warnings
            }
            snapshot.connection = .connected
            snapshot.lastUpdated = now
            backoff.reset()
        } catch {
            markDisconnected(error, now: now)
        }
        return snapshot
    }

    /// Adds per-container GPU usage to the samples. Containers whose IDs appear in the GPU
    /// process list get their share; when attribution was complete the rest are known to be at 0,
    /// otherwise they stay unknown (nil).
    static func attributeGPU(_ gpu: GPUSample, to samples: inout [String: ResourceSample]) {
        let usage = gpu.usageByContainer()
        for id in samples.keys {
            if let entry = usage[id] {
                samples[id]?.gpuPercent = entry.utilization
                samples[id]?.gpuMemory = entry.memory
            } else if gpu.processAttributionComplete {
                samples[id]?.gpuPercent = 0
                samples[id]?.gpuMemory = 0
            }
        }
    }

    private func markDisconnected(_ error: Error, now: Double) {
        let random = self.random
        let delay = backoff.nextDelay(random: random)
        snapshot.connection = .disconnected(message: Self.describe(error), retryAt: now + delay)
        // Stale samples would draw misleading flat lines; drop rates but keep the lists for context.
        sampler = StatsSampler()
        snapshot.version = nil
    }

    /// Seconds until the next reconnect attempt is due (0 when connected).
    public func retryDelay() -> Double {
        if case .disconnected(_, let retryAt?) = snapshot.connection {
            return max(0, retryAt - clock())
        }
        return 0
    }

    /// Fetches stats with bounded concurrency; individual failures (a container that just
    /// stopped) are skipped rather than failing the round.
    func fetchStats(ids: [String]) async -> [String: ContainerStats] {
        let api = self.api
        let limit = statsConcurrency
        return await withTaskGroup(of: (String, ContainerStats?).self) { group in
            var results: [String: ContainerStats] = [:]
            var iterator = ids.makeIterator()
            var running = 0
            while running < limit, let id = iterator.next() {
                group.addTask { (id, try? await api.stats(id: id)) }
                running += 1
            }
            while let (id, stats) = await group.next() {
                if let stats { results[id] = stats }
                if let next = iterator.next() {
                    group.addTask { (next, try? await api.stats(id: next)) }
                }
            }
            return results
        }
    }

    /// Loads the detail pane data for one container.
    @discardableResult
    public func loadDetail(containerID: String, pane: DetailPane, isActive: Bool) async -> ContainerDetailData {
        var data = snapshot.detail?.containerID == containerID ? snapshot.detail! : ContainerDetailData(containerID: containerID)
        data.error = nil
        do {
            switch pane {
            case .overview:
                data.details = try await api.inspectContainer(id: containerID)
            case .processes:
                if isActive {
                    data.processes = ProcessTree.build(ProcessTree.parse(try await api.top(id: containerID)))
                } else {
                    data.processes = []
                }
            case .logs:
                data.logs = try await api.logs(id: containerID, tail: logTail)
            }
        } catch {
            data.error = Self.describe(error)
        }
        data.loadedAt = clock()
        snapshot.detail = data
        return data
    }

    public func clearDetail() {
        snapshot.detail = nil
    }

    /// Executes a management operation and reports a human-readable outcome.
    public func perform(_ operation: DockerOperation) async -> OperationResult {
        do {
            let message = try await execute(operation)
            return OperationResult(operation: operation, succeeded: true, message: message)
        } catch {
            return OperationResult(operation: operation, succeeded: false, message: Self.describe(error))
        }
    }

    private func execute(_ operation: DockerOperation) async throws -> String {
        switch operation {
        case .container(let action, let id, _):
            try await api.perform(action, containerID: id)
        case .removeContainer(let id, _, let force):
            try await api.removeContainer(id: id, force: force, removeVolumes: false)
            if snapshot.detail?.containerID == id { snapshot.detail = nil }
        case .compose(let composeOperation, let projectName):
            return try await executeCompose(composeOperation, projectName: projectName)
        case .removeImage(let id, _, let force):
            try await api.removeImage(id: id, force: force)
        case .removeNetwork(let id, _):
            try await api.removeNetwork(id: id)
        case .removeVolume(let name):
            try await api.removeVolume(name: name, force: false)
        case .prune(let target):
            let report: PruneReport
            switch target {
            case .containers: report = try await api.pruneContainers()
            case .images: report = try await api.pruneImages(danglingOnly: true)
            case .networks: report = try await api.pruneNetworks()
            case .volumes: report = try await api.pruneVolumes()
            }
            return "\(operation.successDescription): \(report.deleted.count) removed, \(Format.bytes(report.spaceReclaimed)) reclaimed"
        }
        return operation.successDescription
    }

    private func executeCompose(_ operation: ComposeOperation, projectName: String) async throws -> String {
        let containers = try await api.listContainers(all: true)
        guard let project = ComposeProject.group(containers).projects.first(where: { $0.name == projectName }) else {
            throw DockerError.http(status: 404, message: "compose project '\(projectName)' has no containers")
        }
        if operation.isEngineNative, let action = operation.containerAction() {
            let targets = project.containers.filter { operation.applies(to: $0) }
            guard !targets.isEmpty else { return "compose \(operation.rawValue) \(projectName): nothing to do" }
            var failures: [String] = []
            for container in targets {
                do {
                    try await api.perform(action, containerID: container.id)
                } catch {
                    failures.append("\(container.name): \(Self.describe(error))")
                }
            }
            if !failures.isEmpty {
                throw DockerError.io("compose \(operation.rawValue) failed for " + failures.joined(separator: "; "))
            }
            return "compose \(operation.rawValue) \(projectName): \(targets.count) container(s)"
        }
        let invocation = ComposeCommandBuilder.invocation(for: operation, project: project, endpoint: endpoint)
        let result = try await commandRunner.run(invocation)
        guard result.succeeded else {
            let lastLine = result.output.split(separator: "\n").last.map(String.init) ?? "exit code \(result.exitCode)"
            throw DockerError.io("\(invocation.displayString) failed: \(lastLine)")
        }
        return "compose \(operation.rawValue) \(projectName) finished"
    }

    static func describe(_ error: Error) -> String {
        if let docker = error as? DockerError { return docker.description }
        return String(describing: error)
    }
}
