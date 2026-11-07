import Testing
@testable import DtopCore

@Suite("Container models")
struct ContainerModelTests {
    @Test("decodes container list rows including null Ports and Labels")
    func list() throws {
        let containers = try Fixtures.decode([Container].self, Fixtures.containerList)
        let web = containers[0]
        #expect(web.shortID == "aaaaaaaaaaaa")
        #expect(web.name == "web-1")
        #expect(web.state == .running)
        #expect(web.health == .healthy)
        #expect(web.composeProject == "shop")
        #expect(web.composeService == "web")
        #expect(web.composeContainerNumber == 1)
        #expect(web.composeWorkingDirectory == "/srv/shop")
        #expect(web.composeConfigFiles == ["/srv/shop/compose.yaml", "/srv/shop/compose.override.yaml"])
        #expect(web.portSummary == "0.0.0.0:8080->80/tcp, [::]:8080->80/tcp")
        let db = containers[1]
        #expect(db.ports.isEmpty)
        #expect(db.labels.isEmpty)
        #expect(db.state == .exited)
        #expect(db.health == nil)
        #expect(db.composeProject == nil)
        #expect(db.composeConfigFiles.isEmpty)
    }

    @Test("names pick the shortest alias and fall back to the short ID")
    func names() {
        let linked = Container(id: "0123456789abcdef", names: ["/app/db", "/db"], image: "x", state: .running)
        #expect(linked.name == "db")
        let unnamed = Container(id: "0123456789abcdef", names: [], image: "x", state: .running)
        #expect(unnamed.name == "0123456789ab")
    }

    @Test("states parse case-insensitively and keep unknown values", arguments: [
        ("running", ContainerState.running), ("Exited", .exited), ("PAUSED", .paused), ("restarting", .restarting),
        ("created", .created), ("removing", .removing), ("dead", .dead)
    ])
    func states(raw: String, expected: ContainerState) {
        #expect(ContainerState(rawValue: raw) == expected)
        #expect(ContainerState(rawValue: expected.rawValue) == expected)
    }

    @Test("unknown states round-trip and are inactive")
    func unknownState() throws {
        let state = ContainerState(rawValue: "hibernating")
        #expect(state == .unknown("hibernating"))
        #expect(state.description == "hibernating")
        #expect(!state.isActive)
        let decoded = try Fixtures.decode([Container].self, #"[{"Id":"x","State":42}]"#)
        #expect(decoded[0].state == .unknown("unknown"))
    }

    @Test("activity and sort rank put live containers first")
    func ranks() {
        #expect(ContainerState.running.isActive)
        #expect(ContainerState.paused.isActive)
        #expect(ContainerState.restarting.isActive)
        #expect(!ContainerState.exited.isActive)
        #expect(!ContainerState.created.isActive)
        let ordered: [ContainerState] = [.dead, .running, .exited, .paused, .created].sorted { $0.sortRank < $1.sortRank }
        #expect(ordered == [.running, .paused, .created, .exited, .dead])
    }

    @Test("health is parsed from the status suffix", arguments: [
        ("Up 3 minutes (healthy)", HealthStatus.healthy as HealthStatus?),
        ("Up 3 minutes (unhealthy)", .unhealthy),
        ("Up 2 seconds (health: starting)", .starting),
        ("Up 3 minutes", nil),
        ("Exited (1) 2 hours ago", nil),
        ("", nil)
    ])
    func health(status: String, expected: HealthStatus?) {
        #expect(HealthStatus.fromStatusText(status) == expected)
    }

    @Test("port display matches the docker CLI")
    func ports() {
        #expect(PortBinding(privatePort: 80).display == "80/tcp")
        #expect(PortBinding(ip: "127.0.0.1", privatePort: 53, publicPort: 5353, type: "udp").display == "127.0.0.1:5353->53/udp")
        #expect(PortBinding(privatePort: 80, publicPort: 8080).display == "0.0.0.0:8080->80/tcp")
    }

    @Test("one-off compose containers are flagged")
    func oneOff() {
        let container = Container(id: "x", names: ["/x"], image: "x", labels: [ComposeLabel.oneOff: "True"], state: .running)
        #expect(container.isComposeOneOff)
    }

    @Test("decodes inspect output for the details panel")
    func inspect() throws {
        let details = try Fixtures.decode(ContainerDetails.self, Fixtures.containerInspect)
        #expect(details.name == "web-1")
        #expect(details.state.status == .running)
        #expect(details.state.pid == 4242)
        #expect(details.state.health == .healthy)
        #expect(details.restartCount == 2)
        #expect(details.restartPolicy == "unless-stopped")
        #expect(details.memoryLimit == 536_870_912)
        #expect(details.nanoCPUs == 1_500_000_000)
        #expect(details.commandLine == "/docker-entrypoint.sh nginx -g daemon off;")
        #expect(details.imageName == "nginx:alpine")
        #expect(details.environment.count == 2)
        #expect(details.networks.map(\.name) == ["bridge", "shop_default"])
        #expect(details.networks.last?.ipAddress == "172.20.0.3")
        #expect(details.portBindings["80/tcp"] == ["0.0.0.0:8080"])
        #expect(details.portBindings["443/tcp"] == [])
        #expect(details.mounts.count == 2)
        #expect(details.mounts[0].name == "shop_data")
        #expect(!details.mounts[1].readWrite)
    }

    @Test("inspect survives a minimal payload")
    func minimalInspect() throws {
        let details = try Fixtures.decode(ContainerDetails.self, #"{"Id":"x","State":{}}"#)
        #expect(details.name == "")
        #expect(details.state.status == .unknown("unknown"))
        #expect(details.state.health == nil)
        #expect(details.networks.isEmpty)
        #expect(details.commandLine == "")
    }
}

@Suite("Stats model")
struct StatsModelTests {
    @Test("decodes cgroup v2 stats, including a UInt64.max pids limit")
    func linux() throws {
        let stats = try Fixtures.decode(ContainerStats.self,
                                        Fixtures.linuxStats(read: "2024-05-01T10:00:00Z", total: 500, system: 10_000))
        #expect(stats.cpu.usage.totalUsage == 500)
        #expect(stats.cpu.systemUsage == 10_000)
        #expect(stats.cpu.onlineCPUs == 4)
        #expect(stats.precpu.systemUsage == nil)
        #expect(stats.memory.usage == 104_857_600)
        #expect(stats.memory.limit == 1_073_741_824)
        #expect(stats.memoryUsageExcludingCache == 104_857_600 - 4_194_304)
        #expect(stats.totalNetwork.rx == 1_000)
        #expect(stats.totalNetwork.tx == 2_000)
        #expect(stats.totalBlockIO.read == 4_096)
        #expect(stats.totalBlockIO.write == 8_192)
        #expect(stats.pidsCurrent == 7)
        #expect(stats.pidsLimit == UInt64.max)
    }

    @Test("cgroup v1 uses total_inactive_file, then cache")
    func cgroupV1() {
        var stats = ContainerStats(cpu: .init(usage: .init(totalUsage: 0)))
        stats.memory = .init(usage: 1000, limit: 4000, stats: ["total_inactive_file": 300, "inactive_file": 100, "cache": 500])
        #expect(stats.memoryUsageExcludingCache == 700)
        stats.memory.stats = ["cache": 500]
        #expect(stats.memoryUsageExcludingCache == 500)
        stats.memory.stats = [:]
        #expect(stats.memoryUsageExcludingCache == 1000)
        stats.memory.stats = ["inactive_file": 5000]
        #expect(stats.memoryUsageExcludingCache == 1000, "cache larger than usage is ignored")
        stats.memory = .init()
        #expect(stats.memoryUsageExcludingCache == 0)
    }

    @Test("block I/O ops are matched case-insensitively (cgroup v1 capitalizes)")
    func blockIOCase() {
        var stats = ContainerStats(cpu: .init(usage: .init(totalUsage: 0)))
        stats.blockIO = [.init(op: "Read", value: 10), .init(op: "Write", value: 20), .init(op: "Total", value: 30),
                         .init(op: "read", value: 1)]
        #expect(stats.totalBlockIO.read == 11)
        #expect(stats.totalBlockIO.write == 20)
    }

    @Test("decodes Windows container stats")
    func windows() throws {
        let stats = try Fixtures.decode(ContainerStats.self, Fixtures.windowsStats)
        #expect(stats.numProcs == 2)
        #expect(stats.cpu.systemUsage == nil)
        #expect(stats.memory.privateWorkingSet == 30_000_000)
        #expect(stats.memoryUsageExcludingCache == 30_000_000)
        #expect(stats.totalBlockIO.read == 1000)
        #expect(stats.totalBlockIO.write == 2000)
        #expect(stats.totalNetwork.rx == 500)
    }

    @Test("CPU count falls back from online_cpus to the per-CPU array, then 1")
    func cpuCount() {
        #expect(ContainerStats.CPUStats(usage: .init(totalUsage: 0), onlineCPUs: 8).effectiveCPUCount == 8)
        #expect(ContainerStats.CPUStats(usage: .init(totalUsage: 0, perCPUUsage: [1, 2, 0]), onlineCPUs: nil)
            .effectiveCPUCount == 2)
        #expect(ContainerStats.CPUStats(usage: .init(totalUsage: 0)).effectiveCPUCount == 1)
        #expect(ContainerStats.CPUStats(usage: .init(totalUsage: 0), onlineCPUs: 0).effectiveCPUCount == 1)
    }

    @Test("an empty stats object (stopped container) decodes to zeros")
    func empty() throws {
        let stats = try Fixtures.decode(ContainerStats.self, "{}")
        #expect(stats.cpu.usage.totalUsage == 0)
        #expect(stats.networks.isEmpty)
        #expect(stats.memoryUsageExcludingCache == 0)
    }

    @Test("numbers sent as floats or strings are accepted")
    func flexibleNumbers() throws {
        let json = #"{"cpu_stats":{"cpu_usage":{"total_usage":1.5e3},"system_cpu_usage":"2000","online_cpus":2.0},"memory_stats":{"usage":12.0}}"#
        let stats = try Fixtures.decode(ContainerStats.self, json)
        #expect(stats.cpu.usage.totalUsage == 1500)
        #expect(stats.cpu.onlineCPUs == 2)
        #expect(stats.memory.usage == 12)
    }
}

@Suite("Resource models")
struct ResourceModelTests {
    @Test("images: tags, digests, dangling detection and display names")
    func images() throws {
        let images = try Fixtures.decode([DockerImage].self, Fixtures.images)
        #expect(images[0].shortID == "1111111111111111".prefix(12).description)
        #expect(images[0].displayName == "nginx:1.25-alpine")
        #expect(!images[0].isDangling)
        #expect(images[0].containers == 1)
        #expect(images[1].repoTags.isEmpty)
        #expect(images[1].displayName == "postgres@<none>")
        #expect(images[1].containers == -1)
        #expect(images[2].isDangling)
        #expect(images[2].repoDigests.isEmpty)
        #expect(images[2].displayName == "<none>")
        #expect(DockerImage(id: "plainid").shortID == "plainid")
    }

    @Test("networks: IPAM, attached containers, built-ins and compose label")
    func networks() throws {
        let networks = try Fixtures.decode([DockerNetwork].self, Fixtures.networks)
        #expect(networks[0].isBuiltIn)
        #expect(networks[0].ipam == [DockerNetwork.IPAMConfig(subnet: "172.17.0.0/16", gateway: "172.17.0.1")])
        #expect(networks[0].containerIDs.isEmpty)
        #expect(!networks[1].isBuiltIn)
        #expect(networks[1].isInternal)
        #expect(networks[1].attachable)
        #expect(networks[1].enableIPv6)
        #expect(networks[1].containerIDs == ["aaaa", "cccc"])
        #expect(networks[1].composeProject == "shop")
        #expect(networks[1].shortID == "net222222222")
    }

    @Test("volumes: usage data, -1 sentinels and anonymous names")
    func volumes() throws {
        let response = try Fixtures.decode(VolumeListResponse.self, Fixtures.volumes)
        #expect(response.volumes.count == 2)
        #expect(response.warnings.isEmpty)
        let named = response.volumes[0]
        #expect(named.id == "shop_data")
        #expect(named.sizeBytes == 2048)
        #expect(named.refCount == 1)
        #expect(named.composeProject == "shop")
        #expect(!named.isAnonymous)
        let anonymous = response.volumes[1]
        #expect(anonymous.isAnonymous)
        #expect(anonymous.sizeBytes == nil)
        #expect(anonymous.refCount == nil)
        #expect(try Fixtures.decode(VolumeListResponse.self, #"{"Volumes":null}"#).volumes.isEmpty)
    }

    @Test("version and info decode")
    func versionInfo() throws {
        let version = try Fixtures.decode(DockerVersion.self, Fixtures.version)
        #expect(version.version == "27.3.1")
        #expect(version.minAPIVersion == "1.24")
        #expect(version.os == "linux")
        let info = try Fixtures.decode(SystemInfo.self, Fixtures.info)
        #expect(info.name == "docker-host")
        #expect(info.memoryTotal == 16_777_216_000)
        #expect(info.containersPaused == 1)
        #expect(info.images == 12)
    }

    @Test("events decode modern and legacy shapes")
    func events() throws {
        let modern = try Fixtures.decode(DockerEvent.self,
            #"{"Type":"container","Action":"exec_start: sh -c ls","Actor":{"ID":"c","Attributes":{"a":"b"}},"time":5,"timeNano":5000}"#)
        #expect(modern.baseAction == "exec_start")
        #expect(!modern.affectsInventory)
        #expect(modern.attributes == ["a": "b"])
        #expect(modern.timeNano == 5000)
        let legacy = try Fixtures.decode(DockerEvent.self, #"{"status":"die","id":"c2","time":7}"#)
        #expect(legacy.action == "die")
        #expect(legacy.actorID == "c2")
    }

    @Test("only inventory-changing events trigger refreshes", arguments: [
        ("container", "start", true), ("container", "die", true), ("container", "health_status: healthy", true),
        ("container", "exec_create", false), ("container", "attach", false), ("image", "pull", true),
        ("network", "connect", true), ("volume", "mount", true), ("plugin", "enable", false)
    ])
    func affectsInventory(type: String, action: String, expected: Bool) {
        #expect(DockerEvent(type: type, action: action, actorID: "x").affectsInventory == expected)
    }

    @Test("top output decodes and tolerates null")
    func top() throws {
        #expect(try Fixtures.decode(ContainerTop.self, Fixtures.top).titles.count == 8)
        #expect(try Fixtures.decode(ContainerTop.self, #"{"Titles":null,"Processes":null}"#).processes.isEmpty)
    }
}
