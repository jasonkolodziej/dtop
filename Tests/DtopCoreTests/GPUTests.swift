import Foundation
import Testing
@testable import DtopCore

// MARK: - Fixtures

enum GPUFixtures {
    static let containerA = String(repeating: "a", count: 64)
    static let containerB = String(repeating: "b", count: 64)

    static let nvidiaDevices = """
    0, GPU-11111111-2222-3333-4444-555555555555, NVIDIA GeForce RTX 3080, 45, 3210, 10240, 61
    1, GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee, NVIDIA A100-SXM4-40GB, [N/A], 512, 40960, [N/A]
    """

    static let nvidiaApps = """
    4242, GPU-11111111-2222-3333-4444-555555555555, 1536
    5151, GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee, [N/A]
    """

    static let nvidiaPmon = """
    # gpu         pid   type     sm    mem    enc    dec    command
    # Idx           #    C/G      %      %      %      %    name
        0       4242     C     40     12      -      -    python train.py
        1       5151     C      -      -      -      -    worker
        0       7777     G      5      1      -      -    Xorg
    """

    /// Verbatim example from Documentation/gpu/i915.rst.
    static let i915Fdinfo = """
    pos:    0
    flags:  0100002
    mnt_id: 21
    drm-driver: i915
    drm-pdev:   0000:00:02.0
    drm-client-id:      7
    drm-engine-render:  9288864723 ns
    drm-engine-copy:    2035071108 ns
    drm-engine-video:   0 ns
    drm-engine-capacity-video:   2
    drm-engine-video-enhance:   0 ns
    """

    /// Verbatim example from drivers/gpu/drm/xe/xe_drm_client.c.
    static let xeFdinfo = """
    pos:    0
    flags:  0100002
    mnt_id: 26
    ino:    685
    drm-driver:     xe
    drm-client-id:  3
    drm-pdev:       0000:03:00.0
    drm-total-system:       0
    drm-shared-system:      0
    drm-active-system:      0
    drm-resident-system:    0
    drm-purgeable-system:   0
    drm-total-gtt:  192 KiB
    drm-shared-gtt: 0
    drm-active-gtt: 0
    drm-resident-gtt:       192 KiB
    drm-total-vram0:        23992 KiB
    drm-shared-vram0:       16 MiB
    drm-active-vram0:       0
    drm-resident-vram0:     23992 KiB
    drm-total-stolen:       0
    drm-shared-stolen:      0
    drm-active-stolen:      0
    drm-resident-stolen:    0
    drm-cycles-rcs: 28257900
    drm-total-cycles-rcs:   7655183225
    drm-cycles-bcs: 0
    drm-total-cycles-bcs:   7655183225
    drm-cycles-vcs: 0
    drm-total-cycles-vcs:   7655183225
    drm-engine-capacity-vcs:        2
    """

    static func amdgpuFdinfo(clientID: Int = 14, gfxNanoseconds: UInt64, vramKiB: UInt64 = 2048) -> String {
        """
        pos:\t0
        drm-driver:\tamdgpu
        drm-client-id:\t\(clientID)
        drm-pdev:\t0000:03:00.0
        pasid:\t32770
        drm-memory-vram:\t\(vramKiB) KiB
        drm-memory-gtt: \t4 KiB
        drm-engine-gfx:\t\(gfxNanoseconds) ns
        drm-engine-compute:\t0 ns
        """
    }

    static func cgroupV1(_ id: String) -> String {
        "12:pids:/docker/\(id)\n1:cpu:/docker/\(id)\n0::/docker/\(id)\n"
    }
}

/// In-memory `/sys` + `/proc`.
final class FakeGPUFileSystem: GPUFileSystem, @unchecked Sendable {
    private let lock = NSLock()
    var files: [String: String] = [:]
    var directories: [String: [String]] = [:]
    var links: [String: String] = [:]
    var unreadable: Set<String> = []

    func read(_ path: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        return files[path]
    }

    func list(_ path: String) -> [String]? {
        lock.lock()
        defer { lock.unlock() }
        return unreadable.contains(path) ? nil : directories[path]
    }

    func readLink(_ path: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        return links[path]
    }

    func set(_ path: String, _ contents: String) {
        lock.lock()
        files[path] = contents
        lock.unlock()
    }

    /// Adds a process with one DRM fd.
    func addDRMProcess(pid: Int, fd: Int = 5, fdinfo: String, cgroup: String?) {
        lock.lock()
        directories["/proc", default: []].append(String(pid))
        directories["/proc/\(pid)/fd", default: []].append(String(fd))
        links["/proc/\(pid)/fd/\(fd)"] = "/dev/dri/renderD128"
        files["/proc/\(pid)/fdinfo/\(fd)"] = fdinfo
        if let cgroup { files["/proc/\(pid)/cgroup"] = cgroup }
        lock.unlock()
    }

    func addCard(_ name: String, vendor: String, pdev: String, extra: [String: String] = [:]) {
        lock.lock()
        directories["/sys/class/drm", default: []].append(name)
        files["/sys/class/drm/\(name)/device/vendor"] = vendor + "\n"
        files["/sys/class/drm/\(name)/device/uevent"] = "DRIVER=x\nPCI_SLOT_NAME=\(pdev)\n"
        for (file, value) in extra { files["/sys/class/drm/\(name)/device/\(file)"] = value }
        lock.unlock()
    }
}

/// Answers `nvidia-smi` invocations from canned output.
final class FakeSMIRunner: CommandRunning, @unchecked Sendable {
    var devices = GPUFixtures.nvidiaDevices
    var apps = GPUFixtures.nvidiaApps
    var pmon = GPUFixtures.nvidiaPmon
    var failDevices = false

    func run(_ invocation: CommandInvocation) async throws -> CommandResult {
        let arguments = invocation.arguments
        if arguments.first?.hasPrefix("--query-gpu") == true {
            return failDevices ? CommandResult(exitCode: 9, output: "NVIDIA-SMI has failed because it couldn't communicate with the NVIDIA driver.\n")
                : CommandResult(exitCode: 0, output: devices)
        }
        if arguments.first?.hasPrefix("--query-compute-apps") == true { return CommandResult(exitCode: 0, output: apps) }
        if arguments.first == "pmon" { return CommandResult(exitCode: 0, output: pmon) }
        return CommandResult(exitCode: 1, output: "unexpected")
    }
}

// MARK: - nvidia-smi

@Suite("nvidia-smi parsing")
struct NvidiaSMITests {
    @Test("devices: values, MiB to bytes, and [N/A] fields")
    func devices() {
        let devices = NvidiaSMI.parseDevices(GPUFixtures.nvidiaDevices)
        #expect(devices.count == 2)
        #expect(devices[0].index == 0)
        #expect(devices[0].device.id == "nvidia:GPU-11111111-2222-3333-4444-555555555555")
        #expect(devices[0].device.name == "NVIDIA GeForce RTX 3080")
        #expect(devices[0].device.utilization == 45)
        #expect(devices[0].device.memoryUsed == UInt64(3210 * 1_048_576))
        #expect(devices[0].device.memoryTotal == UInt64(10240 * 1_048_576))
        #expect(devices[0].device.temperature == 61)
        #expect(devices[1].device.utilization == nil, "[N/A] becomes nil, not 0")
        #expect(devices[1].device.temperature == nil)
    }

    @Test("garbage and blank lines are skipped")
    func garbage() {
        #expect(NvidiaSMI.parseDevices("\nnot,a,gpu\nx, y\n").isEmpty)
        #expect(NvidiaSMI.parseComputeApps("No running processes found\n").isEmpty)
    }

    @Test("compute apps map pid to device and memory")
    func apps() {
        let apps = NvidiaSMI.parseComputeApps(GPUFixtures.nvidiaApps)
        #expect(apps.count == 2)
        #expect(apps[0].pid == 4242)
        #expect(apps[0].memoryBytes == UInt64(1536 * 1_048_576))
        #expect(apps[1].memoryBytes == nil)
    }

    @Test("pmon rows use the header's column positions; '-' means no sample")
    func pmon() {
        let rows = NvidiaSMI.parseProcessMonitor(GPUFixtures.nvidiaPmon)
        #expect(rows.count == 3)
        #expect(rows[0].index == 0 && rows[0].pid == 4242 && rows[0].utilization == 40)
        #expect(rows[1].utilization == nil)
        #expect(rows[2].pid == 7777)
        #expect(NvidiaSMI.parseProcessMonitor("# gpu pid type sm\n# Idx # C/G %\n").isEmpty)
    }

    @Test("invocations ask for machine-readable output")
    func arguments() {
        #expect(NvidiaSMI.deviceQueryArguments.contains("--format=csv,noheader,nounits"))
        #expect(NvidiaSMI.computeAppsArguments.contains("--format=csv,noheader,nounits"))
        #expect(NvidiaSMI.processMonitorArguments == ["pmon", "-c", "1", "-s", "u"])
        #expect(NvidiaSMI.number("[Not Supported]") == nil)
        #expect(NvidiaSMI.number(" 12.5 ") == 12.5)
    }
}

// MARK: - DRM fdinfo

@Suite("DRM fdinfo")
struct DRMFdinfoTests {
    @Test("parses the kernel's i915 example")
    func i915() throws {
        let client = try #require(DRMClient.parse(GPUFixtures.i915Fdinfo))
        #expect(client.driver == "i915")
        #expect(client.clientID == "7")
        #expect(client.pdev == "0000:00:02.0")
        #expect(client.engineNanoseconds == ["render": 9_288_864_723, "copy": 2_035_071_108, "video": 0, "video-enhance": 0])
        #expect(client.engineCapacity == ["video": 2])
        #expect(client.residentMemory.isEmpty)
        #expect(client.key == "0000:00:02.0#7")
    }

    @Test("parses the kernel's xe example: cycles and per-region memory with units")
    func xe() throws {
        let client = try #require(DRMClient.parse(GPUFixtures.xeFdinfo))
        #expect(client.driver == "xe")
        #expect(client.cycles["rcs"] == 28_257_900)
        #expect(client.totalCycles["rcs"] == 7_655_183_225)
        #expect(client.engineCapacity["vcs"] == 2)
        #expect(client.residentMemory["vram0"] == UInt64(23992 * 1024))
        #expect(client.residentMemory["gtt"] == UInt64(192 * 1024))
        #expect(client.deviceMemory == UInt64(23992 * 1024), "only device-local regions count as GPU memory")
        #expect(client.engineNanoseconds.isEmpty)
    }

    @Test("amdgpu's legacy drm-memory-* keys alias resident memory")
    func amdgpuLegacy() throws {
        let client = try #require(DRMClient.parse(GPUFixtures.amdgpuFdinfo(gfxNanoseconds: 5_000)))
        #expect(client.residentMemory["vram"] == UInt64(2048 * 1024))
        #expect(client.deviceMemory == UInt64(2048 * 1024))
        #expect(client.engineNanoseconds["gfx"] == 5_000)
        let both = try #require(DRMClient.parse("drm-driver: amdgpu\ndrm-client-id: 1\ndrm-resident-vram: 1 MiB\ndrm-memory-vram: 9 MiB\n"))
        #expect(both.residentMemory["vram"] == 1_048_576, "the standard key wins over the deprecated alias")
    }

    @Test("requires drm-driver and drm-client-id")
    func mandatory() {
        #expect(DRMClient.parse("drm-driver: amdgpu\n") == nil)
        #expect(DRMClient.parse("drm-client-id: 3\n") == nil)
        #expect(DRMClient.parse("") == nil)
        #expect(DRMClient.parseBytes("7 GiB") == UInt64(7 * 1_073_741_824))
        #expect(DRMClient.parseBytes("12") == 12)
        #expect(DRMClient.parseBytes("x KiB") == nil)
    }
}

@Suite("DRM usage tracker")
struct DRMUsageTrackerTests {
    func client(_ id: String = "1", engines: [String: UInt64] = [:], capacity: [String: UInt64] = [:],
                cycles: [String: UInt64] = [:], total: [String: UInt64] = [:], pdev: String = "0000:03:00.0") -> DRMClient {
        DRMClient(driver: "x", clientID: id, pdev: pdev, engineNanoseconds: engines, engineCapacity: capacity,
                  cycles: cycles, totalCycles: total, residentMemory: [:])
    }

    @Test("engine nanoseconds over wall time give utilization; the first sample has no baseline")
    func nanoseconds() {
        var tracker = DRMUsageTracker()
        #expect(tracker.update([client(engines: ["gfx": 0, "compute": 0])], now: 10).isEmpty)
        let usage = tracker.update([client(engines: ["gfx": 500_000_000, "compute": 100_000_000])], now: 11)
        let entry = usage["0000:03:00.0#1"]
        #expect(entry?.engines["gfx"] == 50)
        #expect(entry?.engines["compute"] == 10)
        #expect(entry?.busiest == 50)
    }

    @Test("engine capacity divides grouped engines")
    func capacity() {
        var tracker = DRMUsageTracker()
        _ = tracker.update([client(engines: ["video": 0], capacity: ["video": 2])], now: 0)
        let usage = tracker.update([client(engines: ["video": 1_000_000_000], capacity: ["video": 2])], now: 1)
        #expect(usage.values.first?.engines["video"] == 50)
    }

    @Test("xe cycles are measured against total cycles")
    func cycles() {
        var tracker = DRMUsageTracker()
        _ = tracker.update([client(cycles: ["rcs": 100], total: ["rcs": 1_000])], now: 0)
        let usage = tracker.update([client(cycles: ["rcs": 350], total: ["rcs": 2_000])], now: 1)
        #expect(usage.values.first?.engines["rcs"] == 25)
    }

    @Test("a counter that goes backwards keeps the previous value (no negative or huge spikes)")
    func regression() {
        var tracker = DRMUsageTracker()
        _ = tracker.update([client(engines: ["gfx": 1_000_000_000])], now: 0)
        let dip = tracker.update([client(engines: ["gfx": 400_000_000])], now: 1)
        #expect(dip.values.first?.engines["gfx"] == 0)
        let recover = tracker.update([client(engines: ["gfx": 1_500_000_000])], now: 2)
        #expect(recover.values.first?.engines["gfx"] == 50, "measured from the retained larger value")
    }

    @Test("values clamp to 100, vanished clients are forgotten, duplicates count once")
    func clampAndForget() {
        var tracker = DRMUsageTracker()
        _ = tracker.update([client(engines: ["gfx": 0])], now: 0)
        let over = tracker.update([client(engines: ["gfx": 9_000_000_000]), client(engines: ["gfx": 1])], now: 1)
        #expect(over.count == 1)
        #expect(over.values.first?.busiest == 100)
        _ = tracker.update([], now: 2)
        #expect(tracker.update([client(engines: ["gfx": 10_000_000_000])], now: 3).isEmpty, "baseline was dropped")
    }

    @Test("device utilization sums clients per engine, caps at 100, takes the busiest engine")
    func device() {
        let usages = [
            DRMClientUsage(engines: ["render": 30, "video": 10]),
            DRMClientUsage(engines: ["render": 25, "video": 70]),
            DRMClientUsage(engines: ["video": 50])
        ]
        #expect(DRMUsageTracker.deviceUtilization(usages) == 100)
        #expect(DRMUsageTracker.deviceUtilization([DRMClientUsage(engines: ["render": 20]),
                                                   DRMClientUsage(engines: ["render": 15])]) == 35)
        #expect(DRMUsageTracker.deviceUtilization([]) == nil)
    }
}

// MARK: - cgroups, Apple, models

@Suite("GPU support types")
struct GPUSupportTests {
    @Test("container IDs come from every common cgroup layout", arguments: [
        (GPUFixtures.cgroupV1(GPUFixtures.containerA), GPUFixtures.containerA as String?),
        ("0::/system.slice/docker-\(GPUFixtures.containerA).scope\n", GPUFixtures.containerA),
        ("0::/docker/\(GPUFixtures.containerB)\n", GPUFixtures.containerB),
        ("0::/machine.slice/libpod-\(GPUFixtures.containerA).scope/container\n", GPUFixtures.containerA),
        ("0::/kubepods/besteffort/pod1234/\(GPUFixtures.containerB)\n", GPUFixtures.containerB),
        ("0::/user.slice/user-1000.slice/session-2.scope\n", nil),
        ("0::/docker/\(String(repeating: "a", count: 63))\n", nil),
        ("0::/docker/\(String(repeating: "A", count: 64))\n", nil),
        ("", nil)
    ])
    func cgroups(text: String, expected: String?) {
        #expect(CgroupContainerID.parse(text) == expected)
    }

    @Test("PCI vendor IDs map to vendors")
    func vendors() {
        #expect(GPUVendor(pciVendorID: "0x10de\n") == .nvidia)
        #expect(GPUVendor(pciVendorID: "0x1002") == .amd)
        #expect(GPUVendor(pciVendorID: "0x8086") == .intel)
        #expect(GPUVendor(pciVendorID: "0x1234") == .unknown)
        #expect(Set(GPUVendor.allCases.map(\.displayName)).count == GPUVendor.allCases.count)
    }

    @Test("Apple IOAccelerator statistics: utilization keys, memory, names, clamping")
    func apple() throws {
        let m1: [String: Any] = [
            "model": "Apple M2 Pro",
            "PerformanceStatistics": ["Device Utilization %": 37, "Renderer Utilization %": 30,
                                      "In use system memory": Int64(734_003_200)] as [String: Any]
        ]
        let device = try #require(AppleGPUStatistics.device(index: 0, properties: m1))
        #expect(device.id == "apple:0")
        #expect(device.vendor == .apple)
        #expect(device.name == "Apple M2 Pro")
        #expect(device.utilization == 37)
        #expect(device.memoryUsed == 734_003_200)
        #expect(device.memoryTotal == nil, "unified memory has no separate VRAM total")

        let older: [String: Any] = ["IOClass": "AMDRadeonX6000", "PerformanceStatistics": ["GPU Activity(%)": 140.0]]
        let fallback = try #require(AppleGPUStatistics.device(index: 1, properties: older))
        #expect(fallback.utilization == 100, "clamped")
        #expect(fallback.name == "AMDRadeonX6000")
        #expect(AppleGPUStatistics.device(index: 0, properties: ["model": "x"]) == nil)
        let unnamed = try #require(AppleGPUStatistics.device(index: 2, properties: ["PerformanceStatistics": [String: Any]()]))
        #expect(unnamed.name == "Apple GPU")
        #expect(unnamed.utilization == nil, "a missing key is unknown, not idle")
    }

    @Test("samples average devices, total memory and attribute usage per container")
    func sampleMath() {
        let sample = GPUSample(
            devices: [GPUDevice(id: "g0", vendor: .nvidia, name: "a", utilization: 80, memoryUsed: 100, memoryTotal: 1000),
                      GPUDevice(id: "g1", vendor: .nvidia, name: "b", utilization: 20, memoryUsed: 50, memoryTotal: 1000),
                      GPUDevice(id: "g2", vendor: .apple, name: "c")],
            processes: [GPUProcessUsage(pid: 1, deviceID: "g0", utilization: 60, memoryBytes: 10, containerID: "c1"),
                        GPUProcessUsage(pid: 2, deviceID: "g1", utilization: 30, memoryBytes: 5, containerID: "c1"),
                        GPUProcessUsage(pid: 3, deviceID: "g0", utilization: nil, memoryBytes: nil, containerID: "c2"),
                        GPUProcessUsage(pid: 4, deviceID: "g0", utilization: 99, memoryBytes: 1, containerID: nil)])
        #expect(sample.averageUtilization == 50)
        #expect(sample.totalMemoryUsed == 150)
        #expect(sample.totalMemory == 2000)
        let usage = sample.usageByContainer()
        #expect(usage["c1"]?.utilization == 30, "(60 + 30) / 3 devices")
        #expect(usage["c1"]?.memory == 15)
        #expect(usage["c2"]?.utilization == 0)
        #expect(usage.count == 2, "host processes are not attributed")
        #expect(GPUSample.empty.averageUtilization == nil)
        #expect(GPUSample.empty.totalMemoryUsed == nil)
    }

    @Test("metrics store keeps host GPU readings and history")
    func metricsStore() {
        var store = MetricsStore(capacity: 4)
        let gpu = GPUSample(devices: [GPUDevice(id: "g", vendor: .amd, name: "R", utilization: 40, memoryUsed: 7)])
        store.record(["c": ResourceSample(gpuPercent: 12, gpuMemory: 3)], timestamp: 1, gpu: gpu)
        #expect(store.gpu == gpu)
        #expect(store.totalLatest.gpuPercent == 40)
        #expect(store.totalLatest.gpuMemory == 7)
        #expect(store.total.gpu.elements == [40])
        #expect(store.series["c"]?.gpu.elements == [12])
        store.record([:], timestamp: 2)
        #expect(store.gpu.devices.isEmpty)
        #expect(store.totalLatest.gpuPercent == nil)
        store.reset()
        #expect(store.gpu == .empty)
    }

    @Test("container inspect reports GPU access from --gpus and device nodes")
    func gpuAccess() throws {
        let json = #"""
        {"Id":"x","State":{},"HostConfig":{
          "DeviceRequests":[{"Driver":"nvidia","Count":-1,"Capabilities":[["gpu"]]},
                            {"Driver":"","Count":0,"DeviceIDs":["0","2"],"Capabilities":[["gpu","compute"]]},
                            {"Driver":"nvidia","Count":1,"Capabilities":[["gpu"]]},
                            {"Driver":"other","Count":1,"Capabilities":[["tpu"]]}],
          "Devices":[{"PathOnHost":"/dev/dri/renderD128","PathInContainer":"/dev/dri/renderD128"},
                     {"PathOnHost":"/dev/kfd"},{"PathOnHost":"/dev/ttyUSB0"}]}}
        """#
        let details = try Fixtures.decode(ContainerDetails.self, json)
        #expect(details.gpuAccess == [
            "nvidia: all GPUs", "gpu: 0, 2", "nvidia: 1 GPU", "devices: /dev/dri/renderD128, /dev/kfd"
        ])
        #expect(try Fixtures.decode(ContainerDetails.self, #"{"Id":"x","State":{},"HostConfig":{"DeviceRequests":null}}"#)
            .gpuAccess.isEmpty)
    }
}

// MARK: - Collector

@Suite("System GPU collector")
struct SystemGPUCollectorTests {
    @Test("NVIDIA on Linux: devices, merged per-process usage, container attribution")
    func nvidiaLinux() async {
        let fs = FakeGPUFileSystem()
        fs.set("/proc/4242/cgroup", GPUFixtures.cgroupV1(GPUFixtures.containerA))
        fs.set("/proc/5151/cgroup", "0::/system.slice/docker-\(GPUFixtures.containerB).scope\n")
        fs.set("/proc/7777/cgroup", "0::/user.slice\n")
        let collector = SystemGPUCollector(platform: .linux, nvidiaSMI: "/usr/bin/nvidia-smi", fileSystem: fs,
                                           runner: FakeSMIRunner())
        let sample = await collector.sample(now: 0)
        #expect(sample.devices.map(\.name) == ["NVIDIA GeForce RTX 3080", "NVIDIA A100-SXM4-40GB"])
        #expect(sample.processes.map(\.pid) == [4242, 5151, 7777])
        let train = sample.processes[0]
        #expect(train.utilization == 40)
        #expect(train.memoryBytes == UInt64(1536 * 1_048_576))
        #expect(train.containerID == GPUFixtures.containerA)
        #expect(sample.processes[1].containerID == GPUFixtures.containerB)
        #expect(sample.processes[2].containerID == nil, "host processes stay unattributed")
        #expect(sample.processes[2].memoryBytes == nil, "graphics process only seen by pmon")
        #expect(sample.processAttributionComplete)
        #expect(sample.issues.isEmpty)
        let usage = sample.usageByContainer()
        #expect(usage[GPUFixtures.containerA]?.utilization == 20, "40% of one of two GPUs")
    }

    @Test("NVIDIA on Windows has no cgroups, so per-container attribution is unknown")
    func nvidiaWindows() async {
        let collector = SystemGPUCollector(platform: .windows, nvidiaSMI: #"C:\Windows\System32\nvidia-smi.exe"#,
                                           fileSystem: FakeGPUFileSystem(), runner: FakeSMIRunner())
        let sample = await collector.sample(now: 0)
        #expect(sample.devices.count == 2)
        #expect(sample.processes.allSatisfy { $0.containerID == nil })
        #expect(!sample.processAttributionComplete)
    }

    @Test("a failing nvidia-smi is reported, not fatal")
    func nvidiaFailure() async {
        let runner = FakeSMIRunner()
        runner.failDevices = true
        let collector = SystemGPUCollector(platform: .linux, nvidiaSMI: "nvidia-smi", fileSystem: FakeGPUFileSystem(), runner: runner)
        let sample = await collector.sample(now: 0)
        #expect(sample.devices.isEmpty)
        #expect(sample.issues.first?.contains("couldn't communicate with the NVIDIA driver") == true)
        #expect(!sample.processAttributionComplete)
    }

    @Test("no GPU at all yields an empty sample")
    func none() async {
        let collector = SystemGPUCollector(platform: .linux, nvidiaSMI: nil, fileSystem: FakeGPUFileSystem(), runner: FakeSMIRunner())
        let sample = await collector.sample(now: 0)
        #expect(sample.devices.isEmpty)
        #expect(sample.issues.isEmpty)
    }

    @Test("AMD: sysfs busy percent and VRAM, per-process usage from fdinfo across two polls")
    func amd() async {
        let fs = FakeGPUFileSystem()
        fs.addCard("card1", vendor: "0x1002", pdev: "0000:03:00.0", extra: [
            "gpu_busy_percent": "63\n", "mem_info_vram_used": "1073741824\n", "mem_info_vram_total": "8589934592\n",
            "product_name": "Radeon RX 7900 XT\n"
        ])
        fs.directories["/sys/class/drm"]?.append(contentsOf: ["card1-DP-1", "renderD128", "version"])
        fs.addDRMProcess(pid: 300, fdinfo: GPUFixtures.amdgpuFdinfo(gfxNanoseconds: 0), cgroup: GPUFixtures.cgroupV1(GPUFixtures.containerA))
        let collector = SystemGPUCollector(platform: .linux, nvidiaSMI: nil, fileSystem: fs, runner: FakeSMIRunner())

        let first = await collector.sample(now: 100)
        #expect(first.devices == [GPUDevice(id: "drm:0000:03:00.0", vendor: .amd, name: "Radeon RX 7900 XT",
                                            utilization: 63, memoryUsed: 1_073_741_824, memoryTotal: 8_589_934_592)])
        #expect(first.processes.first?.utilization == nil, "no baseline yet")
        #expect(first.processes.first?.memoryBytes == UInt64(2048 * 1024))
        #expect(first.processes.first?.containerID == GPUFixtures.containerA)

        fs.set("/proc/300/fdinfo/5", GPUFixtures.amdgpuFdinfo(gfxNanoseconds: 750_000_000))
        let second = await collector.sample(now: 101)
        #expect(second.processes.first?.utilization == 75)
        #expect(second.processAttributionComplete)
    }

    @Test("Intel: device utilization is derived from its clients; one client behind two fds counts once")
    func intel() async {
        let fs = FakeGPUFileSystem()
        fs.addCard("card0", vendor: "0x8086", pdev: "0000:00:02.0")
        let fdinfo = { (render: UInt64) in
            "drm-driver: i915\ndrm-pdev: 0000:00:02.0\ndrm-client-id: 7\ndrm-engine-render: \(render) ns\n"
        }
        fs.addDRMProcess(pid: 10, fd: 4, fdinfo: fdinfo(0), cgroup: nil)
        fs.addDRMProcess(pid: 10, fd: 9, fdinfo: fdinfo(0), cgroup: nil)
        fs.directories["/proc"] = ["10"]
        let collector = SystemGPUCollector(platform: .linux, nvidiaSMI: nil, fileSystem: fs, runner: FakeSMIRunner())
        _ = await collector.sample(now: 0)
        fs.set("/proc/10/fdinfo/4", fdinfo(300_000_000))
        fs.set("/proc/10/fdinfo/9", fdinfo(300_000_000))
        let sample = await collector.sample(now: 1)
        #expect(sample.devices.first?.vendor == .intel)
        #expect(sample.devices.first?.name == "Intel GPU 0000:00:02.0")
        #expect(sample.devices.first?.utilization == 30, "not doubled by the second fd")
        #expect(sample.devices.first?.memoryUsed == nil, "integrated GPU without VRAM")
        #expect(sample.processes.count == 1)
    }

    @Test("unreadable processes make attribution incomplete and explain why")
    func permissions() async {
        let fs = FakeGPUFileSystem()
        fs.addCard("card0", vendor: "0x1002", pdev: "0000:03:00.0")
        fs.directories["/proc"] = ["1", "2", "self"]
        fs.directories["/proc/2/fd"] = []
        fs.unreadable = ["/proc/1/fd"]
        let collector = SystemGPUCollector(platform: .linux, nvidiaSMI: nil, fileSystem: fs, runner: FakeSMIRunner())
        let sample = await collector.sample(now: 0)
        #expect(!sample.processAttributionComplete)
        #expect(sample.issues == ["per-container AMD/Intel GPU usage needs root (1 processes were not readable)"])
    }

    @Test("NVIDIA cards in sysfs are left to nvidia-smi; card entries are recognised")
    func cardFiltering() async {
        let fs = FakeGPUFileSystem()
        fs.addCard("card0", vendor: "0x10de", pdev: "0000:01:00.0")
        let collector = SystemGPUCollector(platform: .linux, nvidiaSMI: nil, fileSystem: fs, runner: FakeSMIRunner())
        #expect(await collector.sample(now: 0).devices.isEmpty)
        #expect(SystemGPUCollector.isCardEntry("card12"))
        #expect(!SystemGPUCollector.isCardEntry("card0-HDMI-A-1"))
        #expect(!SystemGPUCollector.isCardEntry("renderD128"))
        #expect(!SystemGPUCollector.isCardEntry("card"))
    }

    @Test("the default collector matches the platform")
    func defaults() {
        #expect(SystemGPUCollector.makeDefault(environment: [:], platform: .linux) is SystemGPUCollector)
        #expect(SystemGPUCollector.makeDefault(environment: [:], platform: .windows) is SystemGPUCollector)
        #expect(SystemGPUCollector.makeDefault(environment: [:], platform: .other) == nil)
        #if canImport(IOKit)
        #expect(SystemGPUCollector.makeDefault(environment: [:], platform: .macOS) is AppleGPUCollector)
        #else
        #expect(SystemGPUCollector.makeDefault(environment: [:], platform: .macOS) == nil)
        #endif
        let found = SystemGPUCollector.makeDefault(environment: ["PATH": "/nonexistent"], platform: .linux) as? SystemGPUCollector
        #expect(found?.nvidiaSMI == nil)
    }

    #if os(Linux)
    @Test("the local file system reads procfs files that report a size of 0")
    func localFileSystem() {
        let fs = LocalGPUFileSystem()
        #expect(fs.read("/proc/self/cgroup")?.isEmpty == false)
        #expect(fs.list("/proc")?.contains("self") == true)
        #expect(fs.readLink("/proc/self/exe") != nil)
        #expect(fs.read("/proc/definitely-missing") == nil)
    }
    #endif
}
