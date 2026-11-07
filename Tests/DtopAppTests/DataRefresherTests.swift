import Testing
@testable import DtopApp
@testable import DtopCore

@Suite("Data refresher")
struct DataRefresherTests {
    func make(_ api: FakeDockerAPI, clock: TestClock = TestClock(), runner: FakeCommandRunner = FakeCommandRunner(),
              backoff: Backoff = Backoff(initialDelay: 1, maximumDelay: 8, multiplier: 2, jitter: 0)) -> DataRefresher {
        DataRefresher(api: api, endpointDescription: "unix:///fake.sock", commandRunner: runner, historySize: 10,
                      statsConcurrency: 2, logTail: 42, backoff: backoff, clock: { clock.now }, random: { 0.5 })
    }

    @Test("a refresh lists containers, samples running ones and records metrics")
    func refresh() async {
        let api = FakeDockerAPI()
        api.containers = [AppFixtures.container("a"), AppFixtures.container("b", state: .exited)]
        api.setStats("id-a", [
            AppFixtures.stats(read: "2024-05-01T10:00:00Z", total: 0, system: 1_000),
            AppFixtures.stats(read: "2024-05-01T10:00:01Z", total: 100, system: 1_200)
        ])
        let refresher = make(api)
        let first = await refresher.refresh(includeInventory: false)
        #expect(first.connection == .connected)
        #expect(first.containers.count == 2)
        #expect(first.version?.version == "27.0.0")
        #expect(first.info?.cpuCount == 4, "info is fetched on the first refresh even without inventory")
        #expect(first.metrics.latest["id-a"]?.cpuPercent == 0)
        #expect(first.metrics.latest["id-b"] == nil, "stopped containers are not sampled")
        #expect(first.images.isEmpty)

        let second = await refresher.refresh(includeInventory: false)
        #expect(second.metrics.latest["id-a"]?.cpuPercent == 100)
        #expect(second.metrics.total.cpu.elements == [0, 100])
        #expect(api.calls.filter { $0 == "version" }.count == 1, "version is only fetched once while connected")
        #expect(!api.calls.contains("stats id-b"))
    }

    @Test("inventory refreshes load images, networks and volumes")
    func inventory() async {
        let api = FakeDockerAPI()
        let snapshot = await make(api).refresh(includeInventory: true)
        #expect(snapshot.images.count == 1)
        #expect(snapshot.networks.count == 1)
        #expect(snapshot.volumes.count == 1)
        #expect(snapshot.warnings.isEmpty)
    }

    @Test("inventory failures become warnings without dropping the connection")
    func inventoryWarnings() async {
        let api = FakeDockerAPI()
        api.inventoryFailure = .http(status: 500, message: "volumes broke")
        let snapshot = await make(api).refresh(includeInventory: true)
        #expect(snapshot.connection == .connected)
        #expect(snapshot.warnings.count == 4)
        #expect(snapshot.warnings.allSatisfy { $0.contains("volumes broke") })
    }

    @Test("failures mark the connection down with an exponential retry schedule, and recover")
    func reconnect() async {
        let api = FakeDockerAPI()
        let clock = TestClock(100)
        let refresher = make(api, clock: clock)
        api.containers = [AppFixtures.container("a")]
        _ = await refresher.refresh(includeInventory: false)

        api.failure = .connectionFailed(endpoint: "unix:///fake.sock", reason: "refused")
        let down = await refresher.refresh(includeInventory: false)
        guard case .disconnected(let message, let retryAt) = down.connection else {
            Issue.record("expected disconnected")
            return
        }
        #expect(message.contains("refused"))
        #expect(retryAt == 101)
        #expect(down.containers.count == 1, "the last known list stays visible")
        #expect(down.version == nil)
        #expect(await refresher.retryDelay() == 1)

        let again = await refresher.refresh(includeInventory: false)
        if case .disconnected(_, let retryAt) = again.connection { #expect(retryAt == 102) }
        clock.advance(1.5)
        #expect(await refresher.retryDelay() == 0.5)

        api.failure = nil
        let back = await refresher.refresh(includeInventory: false)
        #expect(back.connection == .connected)
        #expect(back.version != nil)
        #expect(await refresher.retryDelay() == 0)
        api.failure = .io("reset")
        let later = await refresher.refresh(includeInventory: false)
        if case .disconnected(_, let retryAt) = later.connection {
            #expect(retryAt == clock.now + 1, "backoff resets after a successful refresh")
        }
    }

    @Test("stats fan-out respects the concurrency limit and tolerates per-container failures")
    func fanOut() async {
        let api = FakeDockerAPI()
        api.containers = (0..<7).map { AppFixtures.container("c\($0)") }
        let snapshot = await make(api).refresh(includeInventory: false)
        #expect(snapshot.metrics.latest.count == 7)
        #expect(api.calls.filter { $0.hasPrefix("stats ") }.count == 7)
    }

    @Test("detail loading fetches the data for each pane")
    func details() async {
        let api = FakeDockerAPI()
        let refresher = make(api)
        let overview = await refresher.loadDetail(containerID: "id-a", pane: .overview, isActive: true)
        #expect(overview.details?.name == "id-a")
        #expect(overview.details?.commandLine == "sh -c run")
        let processes = await refresher.loadDetail(containerID: "id-a", pane: .processes, isActive: true)
        #expect(processes.processes.map(\.process.command) == ["init", "worker"])
        #expect(processes.details != nil, "panes accumulate data for the same container")
        let logs = await refresher.loadDetail(containerID: "id-a", pane: .logs, isActive: true)
        #expect(logs.logs.count == 2)
        #expect(api.calls.contains("logs id-a 42"))
        let inactive = await refresher.loadDetail(containerID: "id-a", pane: .processes, isActive: false)
        #expect(inactive.processes.isEmpty)
        let other = await refresher.loadDetail(containerID: "id-b", pane: .logs, isActive: true)
        #expect(other.details == nil, "switching containers starts fresh")
        #expect(await refresher.snapshot.detail?.containerID == "id-b")
        await refresher.clearDetail()
        #expect(await refresher.snapshot.detail == nil)
    }

    @Test("detail errors are captured for display")
    func detailError() async {
        let api = FakeDockerAPI()
        api.failure = .http(status: 404, message: "No such container")
        let data = await make(api).loadDetail(containerID: "gone", pane: .overview, isActive: true)
        #expect(data.error?.contains("No such container") == true)
    }

    @Test("container operations call the matching API")
    func containerOperations() async {
        let api = FakeDockerAPI()
        let refresher = make(api)
        let stop = await refresher.perform(.container(.stop(timeoutSeconds: nil), id: "id-a", name: "a"))
        #expect(stop.succeeded)
        #expect(stop.message == "Stopped a")
        _ = await refresher.perform(.removeContainer(id: "id-a", name: "a", force: true))
        _ = await refresher.perform(.removeImage(id: "sha256:x", name: "x", force: false))
        _ = await refresher.perform(.removeNetwork(id: "n9", name: "net"))
        _ = await refresher.perform(.removeVolume(name: "vol"))
        #expect(api.calls == ["stop id-a", "rm id-a force=true", "rmi sha256:x", "rm network n9", "rm volume vol"])
    }

    @Test("prune reports counts and reclaimed space")
    func prune() async {
        let api = FakeDockerAPI()
        let refresher = make(api)
        let result = await refresher.perform(.prune(.containers))
        #expect(result.message == "Pruned containers: 2 removed, 2.0KiB reclaimed")
        _ = await refresher.perform(.prune(.images))
        _ = await refresher.perform(.prune(.networks))
        _ = await refresher.perform(.prune(.volumes))
        #expect(api.calls == ["prune containers", "prune images dangling=true", "prune networks", "prune volumes"])
    }

    @Test("failed operations report the daemon's message")
    func failure() async {
        let api = FakeDockerAPI()
        api.failure = .http(status: 409, message: "container is paused")
        let result = await make(api).perform(.container(.start, id: "id-a", name: "a"))
        #expect(!result.succeeded)
        #expect(result.message.contains("container is paused"))
    }

    @Test("engine-native compose operations target only applicable containers")
    func composeEngine() async {
        let api = FakeDockerAPI()
        api.containers = [
            AppFixtures.container("web", project: "shop", service: "web"),
            AppFixtures.container("db", state: .exited, project: "shop", service: "db"),
            AppFixtures.container("other", project: "blog", service: "x")
        ]
        let refresher = make(api)
        let start = await refresher.perform(.compose(.start, project: "shop"))
        #expect(start.succeeded)
        #expect(api.calls.filter { !$0.hasPrefix("list") } == ["start id-db"])
        #expect(start.message.contains("1 container"))
        let stop = await refresher.perform(.compose(.stop, project: "shop"))
        #expect(stop.succeeded)
        #expect(api.calls.last == "stop id-web")
        let pause = await refresher.perform(.compose(.unpause, project: "shop"))
        #expect(pause.message.contains("nothing to do"))
    }

    @Test("compose failures are aggregated per container")
    func composePartialFailure() async {
        let api = FakeDockerAPI()
        api.containers = [AppFixtures.container("web", project: "shop"), AppFixtures.container("api", project: "shop")]
        api.failOperation(on: "id-api", with: .http(status: 500, message: "boom"))
        let result = await make(api).perform(.compose(.restart, project: "shop"))
        #expect(!result.succeeded)
        #expect(result.message.contains("api"))
        #expect(result.message.contains("boom"))
        #expect(api.calls.contains("restart id-web"), "other containers are still restarted")
    }

    @Test("CLI compose operations shell out with the project's labels")
    func composeCLI() async {
        let api = FakeDockerAPI()
        api.containers = [AppFixtures.container("web", project: "shop", service: "web")]
        let runner = FakeCommandRunner()
        let refresher = make(api, runner: runner)
        let up = await refresher.perform(.compose(.up, project: "shop"))
        #expect(up.succeeded)
        #expect(runner.invocations.first?.arguments == [
            "compose", "--project-name", "shop", "--project-directory", "/srv/shop", "--file", "/srv/shop/compose.yaml",
            "up", "--detach"
        ])
        runner.result = CommandResult(exitCode: 1, output: "pulling\nno such file compose.yaml\n")
        let down = await refresher.perform(.compose(.down, project: "shop"))
        #expect(!down.succeeded)
        #expect(down.message.contains("no such file compose.yaml"))
    }

    @Test("compose operations on a vanished project fail clearly")
    func composeMissing() async {
        let result = await make(FakeDockerAPI()).perform(.compose(.stop, project: "ghost"))
        #expect(!result.succeeded)
        #expect(result.message.contains("ghost"))
    }

    @Test("error descriptions prefer DockerError's actionable text")
    func describe() {
        #expect(DataRefresher.describe(DockerError.cancelled) == "Request was cancelled")
        struct Custom: Error {}
        #expect(DataRefresher.describe(Custom()).contains("Custom"))
    }
}
