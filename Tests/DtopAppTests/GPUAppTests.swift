import Foundation
import Testing
@testable import DtopApp
@testable import DtopCore
@testable import DtopUI

/// Returns a fixed GPU sample.
struct FakeGPUCollector: GPUCollecting {
    var result: GPUSample
    func sample(now: Double) async -> GPUSample { result }
}

enum GPUAppFixtures {
    static let device = GPUDevice(id: "nvidia:GPU-1", vendor: .nvidia, name: "NVIDIA GeForce RTX 3080",
                                  utilization: 64, memoryUsed: 3 * 1_073_741_824, memoryTotal: 10 * 1_073_741_824)

    /// The standard app snapshot plus one GPU and GPU usage for `web-1`.
    static func snapshot(complete: Bool = true) -> DockerSnapshot {
        var snapshot = AppFixtures.snapshot()
        var samples = snapshot.metrics.latest
        samples["id-web-1"]?.gpuPercent = 48.5
        samples["id-web-1"]?.gpuMemory = 2 * 1_073_741_824
        if complete { samples["id-cache"]?.gpuPercent = 0 }
        var metrics = MetricsStore(capacity: 10)
        metrics.record(samples, timestamp: 1, gpu: GPUSample(devices: [device], processAttributionComplete: complete))
        snapshot.metrics = metrics
        return snapshot
    }
}

@Suite("GPU in the data refresher")
struct GPURefresherTests {
    func refresher(_ gpu: GPUSample?, api: FakeDockerAPI) -> DataRefresher {
        DataRefresher(api: api, endpointDescription: "unix:///x", commandRunner: FakeCommandRunner(),
                      gpuCollector: gpu.map { FakeGPUCollector(result: $0) }, historySize: 10, clock: { 1_000 })
    }

    @Test("GPU usage is attributed to containers; complete attribution means others are at 0")
    func attribution() async {
        let api = FakeDockerAPI()
        api.containers = [AppFixtures.container("trainer"), AppFixtures.container("idle")]
        let gpu = GPUSample(devices: [GPUAppFixtures.device],
                            processes: [GPUProcessUsage(pid: 1, deviceID: "nvidia:GPU-1", utilization: 70,
                                                        memoryBytes: 1024, containerID: "id-trainer")],
                            processAttributionComplete: true)
        let snapshot = await refresher(gpu, api: api).refresh(includeInventory: false)
        #expect(snapshot.metrics.latest["id-trainer"]?.gpuPercent == 70)
        #expect(snapshot.metrics.latest["id-trainer"]?.gpuMemory == 1024)
        #expect(snapshot.metrics.latest["id-idle"]?.gpuPercent == 0)
        #expect(snapshot.metrics.gpu.devices == [GPUAppFixtures.device])
        #expect(snapshot.metrics.totalLatest.gpuPercent == 64)
        #expect(snapshot.metrics.total.gpu.elements == [64])
    }

    @Test("incomplete attribution leaves other containers unknown, not 0")
    func incomplete() async {
        let api = FakeDockerAPI()
        api.containers = [AppFixtures.container("idle")]
        let gpu = GPUSample(devices: [GPUAppFixtures.device], processAttributionComplete: false, issues: ["needs root"])
        let snapshot = await refresher(gpu, api: api).refresh(includeInventory: false)
        #expect(snapshot.metrics.latest["id-idle"]?.gpuPercent == nil)
        #expect(snapshot.metrics.gpu.issues == ["needs root"])
    }

    @Test("without a GPU or collector nothing GPU-related is set")
    func noGPU() async {
        let api = FakeDockerAPI()
        api.containers = [AppFixtures.container("a")]
        for gpu in [nil, GPUSample.empty] {
            let snapshot = await refresher(gpu, api: api).refresh(includeInventory: false)
            #expect(snapshot.metrics.latest["id-a"]?.gpuPercent == nil)
            #expect(snapshot.metrics.gpu.devices.isEmpty)
            #expect(snapshot.metrics.totalLatest.gpuPercent == nil)
        }
    }
}

@Suite("GPU in the views")
struct GPUViewTests {
    func render(_ snapshot: DockerSnapshot, state: AppState = AppState(), columns: Int = 150, rows: Int = 40) -> RenderedFrame {
        AppView.render(RenderContext(state: state, snapshot: snapshot, theme: .harbor,
                                     size: TerminalSize(columns: columns, rows: rows), now: 1_000))
    }

    @Test("a GPU box with name, utilization and memory joins the metrics row")
    func gpuBox() {
        let frame = render(GPUAppFixtures.snapshot())
        let text = frame.buffer.plainText
        #expect(text.contains("─ gpu ─"))
        #expect(text.contains("64% · 3.0GiB / 10GiB"), "the numbers are never truncated at this width")
        #expect(text.contains("GeForce RTX 3080"), "the model name is shown inside the box")
        #expect(text.contains("net"), "wide terminals keep the network box")
        #expect(frame.buffer.text(row: 0).contains("1 gpu"))
        let narrow = render(GPUAppFixtures.snapshot(), columns: 100).buffer.plainText
        #expect(narrow.contains("─ gpu ─"))
        #expect(narrow.contains("64% · 3.0GiB / 10GiB"), "the numbers survive in narrower terminals")
        #expect(!narrow.contains("─ net"), "below 120 columns the GPU box takes the network box's place")
    }

    @Test("the GPU graph is drawn in the theme's gpu color")
    func gpuColor() {
        let frame = render(GPUAppFixtures.snapshot())
        guard let titleRow = frame.buffer.lines.firstIndex(where: { $0.contains("─ gpu ─") }),
              let range = frame.buffer.lines[titleRow].range(of: "gpu ─") else {
            Issue.record("GPU box title not found")
            return
        }
        let column = frame.buffer.lines[titleRow].distance(from: frame.buffer.lines[titleRow].startIndex, to: range.lowerBound)
        #expect(frame.buffer[column, titleRow].style.foreground == Theme.harbor[.gpu])
    }

    @Test("multiple GPUs show a count and the average")
    func multiGPU() {
        var snapshot = GPUAppFixtures.snapshot()
        var second = GPUAppFixtures.device
        second.id = "drm:0000:03:00.0"
        second.vendor = .amd
        second.utilization = 16
        snapshot.metrics.record(snapshot.metrics.latest, timestamp: 2,
                                gpu: GPUSample(devices: [GPUAppFixtures.device, second]))
        let text = render(snapshot).buffer.plainText
        #expect(text.contains("gpu ×2"))
        #expect(text.contains("40%"))
    }

    @Test("GPU collection issues are shown in the GPU box")
    func issue() {
        var snapshot = GPUAppFixtures.snapshot()
        snapshot.metrics.record(snapshot.metrics.latest, timestamp: 2,
                                gpu: GPUSample(devices: [GPUAppFixtures.device], issues: ["per-container needs root"]))
        #expect(render(snapshot).buffer.plainText.contains("per-container needs root"))
    }

    @Test("no GPU: no GPU box, column or header fact")
    func noGPU() {
        let text = render(AppFixtures.snapshot()).buffer.plainText
        #expect(!text.contains("GPU%"))
        #expect(!text.contains("─ gpu"))
        #expect(!text.contains(" gpu "))
    }

    @Test("the container table gains a GPU% column with per-container values")
    func column() {
        let frame = render(GPUAppFixtures.snapshot())
        #expect(frame.buffer.plainText.contains("GPU%"))
        let webRow = frame.buffer.lines.first { $0.contains("web-1") && $0.contains("nginx") } ?? ""
        #expect(webRow.contains("49"), "48.5% rounds to 49")
        let unknown = render(GPUAppFixtures.snapshot(complete: false)).buffer.lines.first { $0.contains("cache") && $0.contains("redis") } ?? ""
        #expect(unknown.contains(" - "), "unknown GPU usage shows '-'")
        #expect(ListView.columns(for: .containers, showGPU: true).map(\.title).contains("GPU%"))
        #expect(!ListView.columns(for: .containers).map(\.title).contains("GPU%"))
        #expect(ListView.columns(for: .images, showGPU: true).map(\.title) == ListView.columns(for: .images).map(\.title))
    }

    @Test("compose project rows sum their containers' GPU share")
    func composeRow() {
        var state = AppState()
        state.tab = .compose
        let projectRow = render(GPUAppFixtures.snapshot(), state: state).buffer.lines.first { $0.contains("▾ shop") } ?? ""
        #expect(projectRow.contains("49"))
    }

    @Test("details show GPU usage, a GPU sparkline and GPU access")
    func details() {
        var snapshot = GPUAppFixtures.snapshot()
        let json = #"{"Id":"id-web-1","Name":"/web-1","State":{"Status":"running","Running":true},"HostConfig":{"DeviceRequests":[{"Driver":"nvidia","Count":-1,"Capabilities":[["gpu"]]}]}}"#
        snapshot.detail = ContainerDetailData(containerID: "id-web-1",
                                              details: try? JSONDecoder().decode(ContainerDetails.self, from: Data(json.utf8)))
        var state = AppState()
        state.selectedIDs[.containers] = "id-web-1"
        let text = render(snapshot, state: state, rows: 50).buffer.plainText
        #expect(text.contains("48.5% · 2.0GiB"))
        #expect(text.contains("nvidia: all GPUs"))
        #expect(text.contains("gpu ▁") || text.contains("gpu ▄") || text.contains("gpu  "), "GPU sparkline row")
    }

    @Test("sorting by GPU puts the biggest GPU users first")
    func sorting() {
        var state = AppState()
        state.sortKey = .gpu
        state.sortDescending = true
        let names = ListModel.items(for: .containers, snapshot: GPUAppFixtures.snapshot(), state: state)
            .compactMap(\.container?.name)
        #expect(names.first == "web-1")
        #expect(SortKey.gpu.prefersDescending)
    }

    @Test("--once prints GPU lines and a GPU% column")
    func onceReport() {
        let text = OnceReport.render(snapshot: GPUAppFixtures.snapshot(), width: 160)
        #expect(text.contains("gpu: NVIDIA GeForce RTX 3080 · 64.0% · 3.0GiB / 10GiB"))
        #expect(text.contains("GPU%"))
        let web = text.split(separator: "\n").first { $0.hasPrefix("web-1") } ?? ""
        #expect(web.contains("48.5"))
        #expect(!OnceReport.render(snapshot: AppFixtures.snapshot(), width: 160).contains("GPU%"))
    }

    @Test("GPU collection can be turned off")
    func configuration() throws {
        var configuration = AppConfiguration()
        #expect(configuration.gpu)
        try configuration.apply(arguments: ["--no-gpu"])
        #expect(!configuration.gpu)
        try configuration.apply(arguments: ["--gpu"])
        #expect(configuration.gpu)
        try configuration.apply(configFile: "gpu = off")
        #expect(!configuration.gpu)
        #expect(AppConfiguration.usage.contains("--no-gpu"))
        try configuration.apply(arguments: ["--sort", "gpu"])
        #expect(AppState(configuration: configuration).sortDescending)
    }

    @Test("short GPU names drop the vendor prefix")
    func shortNames() {
        #expect(MetricsView.shortGPUName(GPUAppFixtures.device) == "GeForce RTX 3080")
        #expect(MetricsView.shortGPUName(GPUDevice(id: "a", vendor: .apple, name: "Apple M2 Pro")) == "M2 Pro")
        #expect(MetricsView.shortGPUName(GPUDevice(id: "b", vendor: .amd, name: "Radeon RX 7900 XT")) == "Radeon RX 7900 XT")
        #expect(MetricsView.shortGPUName(GPUDevice(id: "c", vendor: .intel, name: "Intel ")) == "Intel ")
    }

    @Test("rendering with GPUs never crashes across sizes and themes")
    func robustness() {
        for (columns, rows) in [(40, 10), (80, 24), (100, 30), (119, 30), (120, 30), (200, 60)] {
            for theme in Theme.builtIn {
                let frame = AppView.render(RenderContext(state: AppState(), snapshot: GPUAppFixtures.snapshot(), theme: theme,
                                                         size: TerminalSize(columns: columns, rows: rows), now: 0))
                #expect(frame.buffer.width == columns)
            }
        }
    }
}
