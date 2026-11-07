import Foundation

/// File access the GPU collector needs, injectable so it can be tested against a fake
/// `/sys` + `/proc` tree.
public protocol GPUFileSystem: Sendable {
    /// File contents, or nil when missing or unreadable.
    func read(_ path: String) -> String?
    /// Directory entries, or nil when missing or unreadable (e.g. another user's `/proc/<pid>/fd`).
    func list(_ path: String) -> [String]?
    func readLink(_ path: String) -> String?
}

public struct LocalGPUFileSystem: GPUFileSystem {
    public init() {}

    public func read(_ path: String) -> String? {
        // procfs/sysfs files report a size of 0, so read to EOF instead of trusting stat().
        guard let handle = FileHandle(forReadingAtPath: path) else { return nil }
        defer { try? handle.close() }
        let data = handle.readDataToEndOfFile()
        return String(decoding: data, as: UTF8.self)
    }

    public func list(_ path: String) -> [String]? {
        try? FileManager.default.contentsOfDirectory(atPath: path)
    }

    public func readLink(_ path: String) -> String? {
        try? FileManager.default.destinationOfSymbolicLink(atPath: path)
    }
}

/// Reads GPUs on Linux (NVIDIA via `nvidia-smi`, AMD via amdgpu sysfs, Intel and AMD per-process
/// via DRM fdinfo) and on Windows (NVIDIA via `nvidia-smi`). Process usage is attributed to
/// Docker containers through `/proc/<pid>/cgroup`, which only works when dtop runs on the
/// Docker host itself.
public final class SystemGPUCollector: GPUCollecting, @unchecked Sendable {
    public let platform: Platform
    public let nvidiaSMI: String?
    private let fileSystem: any GPUFileSystem
    private let runner: any CommandRunning
    private let procRoot: String
    private let drmRoot: String
    // Guards `tracker`; samples may be requested from different tasks.
    private let lock = NSLock()
    private var tracker = DRMUsageTracker()

    public init(
        platform: Platform = .current,
        nvidiaSMI: String?,
        fileSystem: any GPUFileSystem = LocalGPUFileSystem(),
        runner: any CommandRunning = ProcessCommandRunner(),
        procRoot: String = "/proc",
        drmRoot: String = "/sys/class/drm"
    ) {
        self.platform = platform
        self.nvidiaSMI = nvidiaSMI
        self.fileSystem = fileSystem
        self.runner = runner
        self.procRoot = procRoot
        self.drmRoot = drmRoot
    }

    /// The collector for this machine, or nil when the platform has no supported GPU source.
    public static func makeDefault(environment: [String: String] = ProcessInfo.processInfo.environment,
                                   platform: Platform = .current) -> (any GPUCollecting)? {
        switch platform {
        case .macOS:
            #if canImport(IOKit)
            return AppleGPUCollector()
            #else
            return nil
            #endif
        case .linux, .windows:
            let smi = ProcessCommandRunner.resolve("nvidia-smi", environment: environment, platform: platform,
                                                   fileExists: { FileManager.default.isExecutableFile(atPath: $0) })
            return SystemGPUCollector(platform: platform, nvidiaSMI: smi)
        case .other:
            return nil
        }
    }

    public func sample(now: Double) async -> GPUSample {
        var result = GPUSample(processAttributionComplete: platform == .linux)
        if let nvidiaSMI {
            await collectNvidia(executable: nvidiaSMI, into: &result)
        }
        if platform == .linux {
            let drm = (try? await runBlocking { [self] in collectDRM(now: now) }) ?? GPUSample()
            result.devices += drm.devices
            result.processes += drm.processes
            result.issues += drm.issues
            if !drm.processAttributionComplete { result.processAttributionComplete = false }
        }
        if result.devices.isEmpty { result.processAttributionComplete = false }
        return result
    }

    // MARK: - NVIDIA

    private func collectNvidia(executable: String, into result: inout GPUSample) async {
        guard let output = await run(executable, NvidiaSMI.deviceQueryArguments, into: &result) else { return }
        let devices = NvidiaSMI.parseDevices(output)
        guard !devices.isEmpty else { return }
        result.devices += devices.map(\.device)

        let apps = (await run(executable, NvidiaSMI.computeAppsArguments, into: &result))
            .map(NvidiaSMI.parseComputeApps) ?? []
        // pmon is optional (unsupported on some GPUs); its absence only loses per-process %.
        let monitor = (try? await runner.run(CommandInvocation(executable: executable, arguments: NvidiaSMI.processMonitorArguments)))
            .flatMap { $0.succeeded ? NvidiaSMI.parseProcessMonitor($0.output) : nil } ?? []

        let uuidByIndex = Dictionary(devices.map { ($0.index, $0.uuid) }, uniquingKeysWith: { first, _ in first })
        var usage: [String: GPUProcessUsage] = [:]
        for app in apps {
            usage["\(app.uuid)#\(app.pid)"] = GPUProcessUsage(pid: app.pid, deviceID: "nvidia:" + app.uuid,
                                                            memoryBytes: app.memoryBytes)
        }
        for row in monitor {
            guard let uuid = uuidByIndex[row.index] else { continue }
            let key = "\(uuid)#\(row.pid)"
            var entry = usage[key] ?? GPUProcessUsage(pid: row.pid, deviceID: "nvidia:" + uuid)
            entry.utilization = row.utilization
            usage[key] = entry
        }
        for var process in usage.values.sorted(by: { $0.pid < $1.pid }) {
            if platform == .linux { process.containerID = containerID(of: process.pid) }
            result.processes.append(process)
        }
        if platform != .linux { result.processAttributionComplete = false }
    }

    private func run(_ executable: String, _ arguments: [String], into result: inout GPUSample) async -> String? {
        do {
            let output = try await runner.run(CommandInvocation(executable: executable, arguments: arguments))
            guard output.succeeded else {
                let reason = output.output.split(separator: "\n").first.map(String.init) ?? "exit code \(output.exitCode)"
                result.issues.append("nvidia-smi failed: \(reason)")
                return nil
            }
            return output.output
        } catch {
            result.issues.append("nvidia-smi could not run: \(error)")
            return nil
        }
    }

    // MARK: - AMD / Intel (DRM)

    struct Card {
        var device: GPUDevice
        var pdev: String
    }

    func drmCards() -> [Card] {
        guard let entries = fileSystem.list(drmRoot) else { return [] }
        var cards: [Card] = []
        var seen = Set<String>()
        for entry in entries.sorted() where Self.isCardEntry(entry) {
            let devicePath = drmRoot + "/" + entry + "/device"
            let vendor = GPUVendor(pciVendorID: fileSystem.read(devicePath + "/vendor") ?? "")
            guard vendor == .amd || vendor == .intel else { continue }
            let uevent = Self.parseKeyValues(fileSystem.read(devicePath + "/uevent") ?? "")
            let pdev = uevent["PCI_SLOT_NAME"] ?? entry
            guard seen.insert(pdev).inserted else { continue }
            let product = fileSystem.read(devicePath + "/product_name")?.trimmingWhitespace()
            let name = product.flatMap { $0.isEmpty ? nil : $0 } ?? "\(vendor.displayName) GPU \(pdev)"
            var device = GPUDevice(id: "drm:" + pdev, vendor: vendor, name: name)
            if vendor == .amd {
                device.utilization = (fileSystem.read(devicePath + "/gpu_busy_percent")?.trimmingWhitespace()).flatMap(Double.init)
                device.memoryUsed = (fileSystem.read(devicePath + "/mem_info_vram_used")?.trimmingWhitespace()).flatMap { UInt64($0) }
                device.memoryTotal = (fileSystem.read(devicePath + "/mem_info_vram_total")?.trimmingWhitespace()).flatMap { UInt64($0) }
            }
            cards.append(Card(device: device, pdev: pdev))
        }
        return cards
    }

    /// `card0` but not connector entries such as `card0-DP-1`.
    static func isCardEntry(_ name: String) -> Bool {
        guard let digits = name.dropPrefix("card"), !digits.isEmpty else { return false }
        return digits.allSatisfy(\.isNumber)
    }

    static func parseKeyValues(_ text: String) -> [String: String] {
        var result: [String: String] = [:]
        for line in text.split(separator: "\n") {
            let parts = line.split(separator: "=", maxSplits: 1)
            if parts.count == 2 { result[String(parts[0])] = String(parts[1]).trimmingWhitespace() }
        }
        return result
    }

    func collectDRM(now: Double) -> GPUSample {
        var cards = drmCards()
        guard !cards.isEmpty else { return GPUSample(processAttributionComplete: true) }

        // Scan every process's open DRM file descriptors (what nvtop and intel_gpu_top do).
        var clients: [DRMClient] = []
        var ownerPID: [String: Int] = [:]
        var unreadable = 0
        for entry in fileSystem.list(procRoot) ?? [] {
            guard let pid = Int(entry) else { continue }
            guard let fds = fileSystem.list("\(procRoot)/\(pid)/fd") else {
                unreadable += 1
                continue
            }
            for fd in fds {
                guard let target = fileSystem.readLink("\(procRoot)/\(pid)/fd/\(fd)"), target.hasPrefix("/dev/dri/"),
                      let text = fileSystem.read("\(procRoot)/\(pid)/fdinfo/\(fd)"),
                      let client = DRMClient.parse(text) else { continue }
                // One client can sit behind several fds (or processes after fork); count it once.
                guard ownerPID[client.key] == nil else { continue }
                ownerPID[client.key] = pid
                clients.append(client)
            }
        }

        lock.lock()
        let usage = tracker.update(clients, now: now)
        lock.unlock()

        var processes: [GPUProcessUsage] = []
        var usageByDevice: [String: [DRMClientUsage]] = [:]
        var memoryByDevice: [String: UInt64] = [:]
        for client in clients {
            guard let pid = ownerPID[client.key] else { continue }
            let clientUsage = usage[client.key]
            if let clientUsage { usageByDevice[client.pdev, default: []].append(clientUsage) }
            memoryByDevice[client.pdev, default: 0] &+= client.deviceMemory
            processes.append(GPUProcessUsage(
                pid: pid,
                deviceID: "drm:" + client.pdev,
                utilization: clientUsage?.busiest,
                memoryBytes: client.deviceMemory > 0 ? client.deviceMemory : nil,
                containerID: containerID(of: pid)
            ))
        }

        // Intel has no busy-percent file: derive device utilization from its clients.
        for index in cards.indices where cards[index].device.vendor == .intel {
            let pdev = cards[index].pdev
            cards[index].device.utilization = DRMUsageTracker.deviceUtilization(usageByDevice[pdev] ?? []) ?? 0
            if let memory = memoryByDevice[pdev], memory > 0 { cards[index].device.memoryUsed = memory }
        }

        var issues: [String] = []
        if unreadable > 0 {
            issues.append("per-container AMD/Intel GPU usage needs root (\(unreadable) processes were not readable)")
        }
        return GPUSample(devices: cards.map(\.device), processes: processes,
                         processAttributionComplete: unreadable == 0, issues: issues)
    }

    func containerID(of pid: Int) -> String? {
        fileSystem.read("\(procRoot)/\(pid)/cgroup").flatMap(CgroupContainerID.parse)
    }
}
