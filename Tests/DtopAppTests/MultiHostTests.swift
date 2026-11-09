import Testing
@testable import DtopApp
@testable import DtopCore
@testable import DtopUI

private func key(_ key: Key, _ modifiers: KeyModifiers = []) -> InputEvent { .key(KeyEvent(key, modifiers: modifiers)) }
private func char(_ character: Character) -> InputEvent { key(.character(character)) }
private func click(_ x: Int, _ y: Int) -> InputEvent { .mouse(MouseEvent(button: .left, action: .press, x: x, y: y)) }

/// A state showing three hosts, the second of them offline.
private func threeHostState() -> AppState {
    var state = AppState()
    state.hostNames = ["local", "prod", "lab"]
    return state
}

private func summaries(active snapshot: DockerSnapshot = AppFixtures.snapshot()) -> [HostSummary] {
    var offline = DockerSnapshot(endpoint: "tcp://prod:2376", connection: .disconnected(message: "connection refused", retryAt: 5))
    offline.containers = []
    return [
        HostSummary(name: "local", description: "unix:///var/run/docker.sock", snapshot: snapshot),
        HostSummary(name: "prod", description: "tcp://prod:2376 (TLS, client cert)", snapshot: offline),
        HostSummary(name: "lab", description: "tcp://lab:2375", snapshot: DockerSnapshot(connection: .connecting))
    ]
}

@Suite("Multi-host configuration")
struct MultiHostConfigurationTests {
    func parse(_ arguments: [String]) throws -> AppConfiguration {
        var configuration = AppConfiguration()
        try configuration.apply(arguments: arguments)
        return configuration
    }

    @Test("--host repeats, takes an optional name, and the first one is still `host`")
    func repeatedHosts() throws {
        let configuration = try parse(["--host", "a=tcp://x:1", "-H", "tcp://y", "--host=unix:///s.sock"])
        #expect(configuration.hosts == [HostRequest(name: "a", address: "tcp://x:1"), HostRequest(address: "tcp://y"),
                                        HostRequest(address: "unix:///s.sock")])
        #expect(configuration.host == "tcp://x:1")
    }

    @Test("contexts, all contexts and the Docker CLI's TLS flags")
    func contextsAndTLS() throws {
        let configuration = try parse(["--context", "prod", "--context=lab", "--all-contexts", "--tlsverify",
                                       "--tlscacert", "/ca.pem", "--tlscert=/c.pem", "--tlskey", "/k.pem", "--tls=false"])
        #expect(configuration.hosts == [HostRequest(context: "prod"), HostRequest(context: "lab")])
        #expect(configuration.allContexts)
        #expect(configuration.globalTLS == TLSRequest(enabled: false, verify: true, caFile: "/ca.pem",
                                                      certificateFile: "/c.pem", keyFile: "/k.pem"))
        #expect(try parse(["--tlsverify=false"]).globalTLS.verify == false)
        #expect(throws: ConfigurationError.self) { try parse(["--tlsverify=maybe"]) }
        #expect(throws: ConfigurationError.self) { try parse(["--tlscacert"]) }
        #expect(throws: ConfigurationError.self) { try parse(["--context"]) }
    }

    @Test("hosts on the command line replace the file's hosts rather than adding to them")
    func commandLineReplacesFile() throws {
        var configuration = AppConfiguration()
        try configuration.apply(configFile: "host = tcp://from-file\ncontext = filectx")
        #expect(configuration.hosts.count == 2)
        try configuration.apply(arguments: ["--host", "tcp://a", "--context", "b"])
        #expect(configuration.hosts == [HostRequest(address: "tcp://a"), HostRequest(context: "b")])
    }

    @Test("dtop.conf: repeatable hosts, global TLS keys and [host NAME] sections")
    func sections() throws {
        var configuration = AppConfiguration()
        try configuration.apply(configFile: """
        tlsverify = true
        cert_path = /certs
        all_contexts = no
        host = local=unix:///var/run/docker.sock

        [host prod]
        endpoint = tcp://prod.example.com:2376
        tlscacert = /prod/ca.pem
        tlscert = /prod/cert.pem
        tlskey = /prod/key.pem

        [host lab]
        context = lab
        tls = off
        """)
        #expect(configuration.globalTLS == TLSRequest(verify: true, certificateDirectory: "/certs"))
        #expect(!configuration.allContexts)
        #expect(configuration.hosts == [
            HostRequest(name: "local", address: "unix:///var/run/docker.sock"),
            HostRequest(name: "prod", address: "tcp://prod.example.com:2376",
                        tls: TLSRequest(caFile: "/prod/ca.pem", certificateFile: "/prod/cert.pem", keyFile: "/prod/key.pem")),
            HostRequest(name: "lab", context: "lab", tls: TLSRequest(enabled: false))
        ])
    }

    @Test("bad sections fail with the line number", arguments: [
        ("[server x]", "line 1"), ("[host]", "line 1"), ("[host a b]", "line 1"), ("[host x", "line 1"),
        ("[host x]\ntheme = dusk", "line 2"), ("[host x]\ntlsverify = sometimes", "line 2"),
        ("[host x]\ntls = on", "needs 'endpoint"),
        ("[host x]\nendpoint = tcp://a\ncontext = b", "sets both 'endpoint' and 'context'")
    ])
    func badSections(text: String, fragment: String) {
        var configuration = AppConfiguration()
        do {
            try configuration.apply(configFile: text)
            Issue.record("expected an error for \(text.debugDescription)")
        } catch {
            #expect(String(describing: error).contains(fragment), "\(error)")
        }
    }

    @Test("an empty host line clears the list; context lines can be cleared too")
    func clearing() throws {
        var configuration = AppConfiguration()
        try configuration.apply(configFile: "host = a=tcp://a\nhost = tcp://b\nhost =\ncontext = x\ncontext =")
        #expect(configuration.hosts.isEmpty)
    }

    @Test("--help documents the new flags")
    func usage() {
        for flag in ["--context", "--all-contexts", "--tls", "--tlsverify", "--tlscacert", "--tlscert", "--tlskey", "[name=]"] {
            #expect(AppConfiguration.usage.contains(flag), "\(flag)")
        }
    }
}

@Suite("Multi-host reducer")
struct MultiHostReducerTests {
    let snapshot = AppFixtures.snapshot()

    @Test("] and [ cycle hosts, wrapping, and reset the per-host selection")
    func cycling() {
        var state = threeHostState()
        state.selectedIDs[.containers] = "id-web-1"
        state.scrollOffsets[.containers] = 4
        state.detailScroll = 7
        #expect(state.handle(char("]"), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.switchHost(1)])
        #expect(state.activeHost == 1)
        #expect(state.selectedIDs.isEmpty)
        #expect(state.scrollOffsets.isEmpty)
        #expect(state.detailScroll == 0)
        #expect(state.notice?.text == "Host: prod")
        #expect(state.handle(char("["), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.switchHost(0)])
        #expect(state.handle(char("["), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.switchHost(2)])
        #expect(state.activeHost == 2)
    }

    @Test("with one host, host keys explain how to add more")
    func singleHost() {
        var state = AppState()
        state.hostNames = ["local"]
        for character: Character in ["]", "[", "H"] {
            #expect(state.handle(char(character), snapshot: snapshot, hitMap: HitMap(), now: 0).isEmpty)
            #expect(state.notice?.text == AppState.singleHostHint)
            #expect(state.mode == .normal)
        }
    }

    @Test("H opens the host list; keys move, Enter and digits switch, Esc closes")
    func picker() {
        var state = threeHostState()
        _ = state.handle(char("H"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .hostPicker(selected: 0))
        _ = state.handle(key(.down), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(char("j"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .hostPicker(selected: 2))
        _ = state.handle(key(.down), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .hostPicker(selected: 0), "wraps around")
        _ = state.handle(char("k"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.handle(key(.enter), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.switchHost(2)])
        #expect(state.mode == .normal)
        #expect(state.activeHost == 2)

        _ = state.handle(char("H"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .hostPicker(selected: 2), "opens on the active host")
        #expect(state.handle(char("2"), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.switchHost(1)])
        _ = state.handle(char("H"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.handle(char("9"), snapshot: snapshot, hitMap: HitMap(), now: 0).isEmpty, "no 9th host")
        #expect(state.handle(key(.enter), snapshot: snapshot, hitMap: HitMap(), now: 0).isEmpty, "choosing the active host changes nothing")
        _ = state.handle(char("H"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(key(.escape), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .normal)
        #expect(state.activeHost == 1)
    }

    @Test("the mouse picks hosts from the header chips and the host list")
    func mouse() {
        var state = threeHostState()
        var hitMap = HitMap()
        hitMap.hostChips = [HostHit(index: 0, rect: Rect(x: 10, y: 0, width: 8, height: 1)),
                            HostHit(index: 2, rect: Rect(x: 20, y: 0, width: 6, height: 1))]
        #expect(state.handle(click(22, 0), snapshot: snapshot, hitMap: hitMap, now: 0) == [.switchHost(2)])
        #expect(state.handle(click(22, 0), snapshot: snapshot, hitMap: hitMap, now: 0).isEmpty, "already active")

        state.mode = .hostPicker(selected: 0)
        var pickerHits = HitMap()
        pickerHits.hostRows = [HostHit(index: 1, rect: Rect(x: 5, y: 8, width: 40, height: 1))]
        let wheel = InputEvent.mouse(MouseEvent(button: .wheelDown, action: .press, x: 0, y: 0))
        _ = state.handle(wheel, snapshot: snapshot, hitMap: pickerHits, now: 0)
        #expect(state.mode == .hostPicker(selected: 1))
        #expect(state.handle(click(9, 8), snapshot: snapshot, hitMap: pickerHits, now: 0) == [.switchHost(1)])
        state.mode = .hostPicker(selected: 0)
        _ = state.handle(click(0, 30), snapshot: snapshot, hitMap: pickerHits, now: 0)
        #expect(state.mode == .normal, "clicking outside closes the list")
    }

    @Test("f follows or pauses the log stream, only in the Logs pane")
    func follow() {
        var state = AppState()
        state.selectedIDs[.containers] = "id-web-1"
        _ = state.handle(char("f"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.logFollow, "outside the Logs pane f only explains itself")
        #expect(state.notice?.text.contains("Logs pane") == true)
        state.detailPane = .logs
        _ = state.handle(char("f"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(!state.logFollow)
        state.detailScroll = 12
        _ = state.handle(char("f"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.logFollow)
        #expect(state.detailScroll == 0, "following again jumps to the newest line")
    }

    @Test("scrolling back in the logs pauses following; scrolling to the end resumes it")
    func wheelFollow() {
        var state = AppState()
        state.selectedIDs[.containers] = "id-web-1"
        state.detailPane = .logs
        var hitMap = HitMap()
        hitMap.detailRect = Rect(x: 50, y: 5, width: 40, height: 20)
        let up = InputEvent.mouse(MouseEvent(button: .wheelUp, action: .press, x: 60, y: 10))
        let down = InputEvent.mouse(MouseEvent(button: .wheelDown, action: .press, x: 60, y: 10))
        _ = state.handle(up, snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(!state.logFollow)
        #expect(state.detailScroll == 3)
        _ = state.handle(down, snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(state.logFollow)
        #expect(state.detailScroll == 0)
        _ = state.handle(up, snapshot: snapshot, hitMap: hitMap, now: 0)
        _ = state.handle(char("v"), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(state.logFollow, "changing panes resets following")
    }
}

@Suite("Multi-host views")
struct MultiHostViewTests {
    func render(_ state: AppState, snapshot: DockerSnapshot = AppFixtures.snapshot(), hosts: [HostSummary] = summaries(),
                columns: Int = 150, rows: Int = 40) -> RenderedFrame {
        AppView.render(RenderContext(state: state, snapshot: snapshot, theme: .harbor,
                                     size: TerminalSize(columns: columns, rows: rows), now: 1_000, hosts: hosts))
    }

    @Test("the header shows a chip per host, with status, and reports where they are")
    func chips() {
        var state = threeHostState()
        state.activeHost = 1
        let frame = render(state)
        let header = frame.buffer.text(row: 0)
        #expect(header.contains(" ● local "))
        #expect(header.contains(" ✕ prod "))
        #expect(header.contains(" ○ lab "))
        #expect(frame.hitMap.hostChips.map(\.index) == [0, 1, 2])
        let prod = frame.hitMap.hostChips[1].rect
        #expect(frame.buffer[prod.x + 1, 0].style.background == Theme.harbor[.tabActive], "the active host is highlighted")
        let local = frame.hitMap.hostChips[0].rect
        #expect(frame.buffer[local.x + 1, 0].style.foreground == Theme.harbor[.statusRunning])
        #expect(frame.buffer.text(row: 39).contains("H hosts"), "the footer mentions the host list")
    }

    @Test("a single host draws no chips; narrow terminals elide the rest")
    func fewOrNarrow() {
        var state = AppState()
        state.hostNames = ["local"]
        let single = render(state, hosts: [summaries()[0]])
        #expect(single.hitMap.hostChips.isEmpty)
        #expect(!single.buffer.text(row: 0).contains("● local"))
        let narrow = render(threeHostState(), columns: 44, rows: 20)
        #expect(narrow.hitMap.hostChips.count < 3)
        #expect(narrow.buffer.width == 44)
    }

    @Test("the host list shows every host with status, load and endpoint")
    func picker() {
        var state = threeHostState()
        state.mode = .hostPicker(selected: 1)
        let frame = render(state)
        let text = frame.buffer.plainText
        #expect(text.contains("hosts (3)"))
        #expect(text.contains("▸ local"), "the active host is marked")
        #expect(text.contains("offline"))
        #expect(text.contains("connecting"))
        #expect(text.contains("tcp://prod:2376 (TLS, client cert)"))
        #expect(text.contains("connection refused"), "the highlighted offline host says why")
        #expect(frame.hitMap.hostRows.map(\.index) == [0, 1, 2])
        let localRow = frame.buffer.lines.first { $0.contains("▸ local") } ?? ""
        #expect(localRow.contains("2/4"), "running/total containers")
    }

    @Test("the log pane reports whether the stream is live, paused or over")
    func logIndicators() {
        var state = AppState()
        state.selectedIDs[.containers] = "id-web-1"
        state.detailPane = .logs
        var snapshot = AppFixtures.snapshot()
        var detail = ContainerDetailData(containerID: "id-web-1", logs: [LogLine(stream: .stdout, text: "GET / 200")])
        detail.logState = .live
        snapshot.detail = detail
        #expect(render(state, snapshot: snapshot, hosts: []).buffer.plainText.contains("● live"))
        state.logFollow = false
        #expect(render(state, snapshot: snapshot, hosts: []).buffer.plainText.contains("⏸ paused · f follow"))
        snapshot.detail?.logState = .ended(nil)
        #expect(render(state, snapshot: snapshot, hosts: []).buffer.plainText.contains("■ ended"))
        snapshot.detail?.logState = .ended("connection reset")
        let lost = render(state, snapshot: snapshot, hosts: []).buffer.plainText
        #expect(lost.contains("✕ stream lost"))
        #expect(lost.contains("log stream lost: connection reset"))
        #expect(lost.contains("GET / 200"), "lines received before the loss stay visible")
        snapshot.detail = ContainerDetailData(containerID: "id-web-1")
        snapshot.detail?.logState = .live
        #expect(render(state, snapshot: snapshot, hosts: []).buffer.plainText.contains("Waiting for log output"))
    }

    @Test("rendering with hosts and the host list never crashes at any size")
    func robustness() {
        var state = threeHostState()
        for mode in [InputMode.normal, .hostPicker(selected: 2)] {
            state.mode = mode
            for (columns, rows) in [(40, 10), (60, 15), (80, 24), (120, 30), (200, 60)] {
                #expect(render(state, columns: columns, rows: rows).buffer.width == columns)
            }
        }
    }
}

@Suite("Multi-host support types")
struct MultiHostSupportTests {
    @Test("--once prints a section per host; one host prints exactly as before")
    func onceReport() {
        let snapshot = AppFixtures.snapshot()
        let single = OnceReport.render(hosts: [("local", snapshot)])
        #expect(single == OnceReport.render(snapshot: snapshot))
        let offline = DockerSnapshot(connection: .disconnected(message: "refused", retryAt: nil))
        let both = OnceReport.render(hosts: [("local", snapshot), ("prod", offline)])
        #expect(both.hasPrefix("== local ==\n"))
        #expect(both.contains("\n== prod ==\ndtop: refused"))
        #expect(OnceReport.render(hosts: []).isEmpty)
    }

    @Test("host summaries count containers and only report load once data arrived")
    func summary() {
        let summary = HostSummary(name: "local", description: "d", snapshot: AppFixtures.snapshot())
        #expect(summary.running == 2)
        #expect(summary.total == 4)
        #expect(summary.memoryTotal == 8_000_000_000)
        #expect(summary.cpuPercent == nil, "the fixture has no lastUpdated")
        var updated = AppFixtures.snapshot()
        updated.lastUpdated = 1
        #expect(HostSummary(name: "local", description: "d", snapshot: updated).cpuPercent == 52.5)
    }

    @Test("streamed logs are capped, dropping the oldest lines")
    func logCap() {
        var detail = ContainerDetailData(containerID: "x")
        let batch = (0..<3_000).map { LogLine(stream: .stdout, text: "line \($0)") }
        #expect(detail.appendLogs(batch) == 0)
        #expect(detail.appendLogs(batch) == 1_000)
        #expect(detail.logs.count == ContainerDetailData.maximumLogLines)
        #expect(detail.logs.first?.text == "line 1000")
        #expect(detail.logs.last?.text == "line 2999")
    }

    @Test("switching hosts keeps tab, sort and filter but resets selection and log position")
    func switchHost() {
        var state = threeHostState()
        state.tab = .images
        state.filterText = "web"
        state.logFollow = false
        state.selectedIndexes[.images] = 2
        state.switchHost(to: 2)
        #expect(state.activeHost == 2)
        #expect(state.tab == .images)
        #expect(state.filterText == "web")
        #expect(state.logFollow)
        #expect(state.selectedIndexes.isEmpty)
        state.switchHost(to: 7)
        #expect(state.activeHost == 2, "out-of-range hosts are ignored")
    }
}

@Suite("Log stream policy")
struct LogStreamPolicyTests {
    let running = LogStreamPolicy.Target(host: 0, containerID: "a", isActive: true)
    let stopped = LogStreamPolicy.Target(host: 0, containerID: "a", isActive: false)

    @Test("no logs on screen: close an open stream, otherwise nothing to do")
    func hidden() {
        #expect(LogStreamPolicy.decide(current: (0, "a"), wanted: nil, state: .live, reconnectPending: false, isReconnect: false) == .stop)
        #expect(LogStreamPolicy.decide(current: nil, wanted: nil, state: nil, reconnectPending: true, isReconnect: true) == .wait)
    }

    @Test("the open stream is kept while it shows the selected container")
    func keep() {
        #expect(LogStreamPolicy.decide(current: (0, "a"), wanted: running, state: .live, reconnectPending: false, isReconnect: false) == .keep)
        #expect(LogStreamPolicy.decide(current: (0, "a"), wanted: running, state: .live, reconnectPending: false, isReconnect: true) == .keep)
    }

    @Test("another container or another host gets a new stream")
    func switching() {
        #expect(LogStreamPolicy.decide(current: (0, "b"), wanted: running, state: nil, reconnectPending: false, isReconnect: false) == .start)
        #expect(LogStreamPolicy.decide(current: (1, "a"), wanted: running, state: .live, reconnectPending: false, isReconnect: false) == .start)
        #expect(LogStreamPolicy.decide(current: nil, wanted: running, state: nil, reconnectPending: false, isReconnect: false) == .start)
    }

    @Test("a stopped container's finished stream is not reopened in a loop")
    func stoppedContainer() {
        #expect(LogStreamPolicy.decide(current: nil, wanted: stopped, state: .ended(nil), reconnectPending: false, isReconnect: false) == .wait)
        #expect(LogStreamPolicy.decide(current: nil, wanted: running, state: .ended(nil), reconnectPending: false, isReconnect: false) == .start,
                "once it runs again, logs stream again")
    }

    @Test("a resumed stream drops lines it already showed; untimed lines are kept")
    func resumeFilter() {
        let batch = [LogLine(stream: .stdout, text: "seen", timestamp: 10), LogLine(stream: .stdout, text: "new", timestamp: 10.5),
                     LogLine(stream: .stderr, text: "untimed")]
        #expect(LogStreamPolicy.newLines(batch, after: 10).map(\.text) == ["new", "untimed"])
        #expect(LogStreamPolicy.newLines(batch, after: nil) == batch)
    }

    @Test("scrolled back, new lines keep the view still, but never past the oldest kept line")
    func scrollClamp() {
        #expect(LogStreamPolicy.scrollBack(10, appended: 5, keptLines: 100) == 15)
        #expect(LogStreamPolicy.scrollBack(4_998, appended: 10, keptLines: 5_000) == 5_000, "the cap dropped the old view")
        #expect(LogStreamPolicy.scrollBack(0, appended: 0, keptLines: 0) == 0)
    }

    @Test("a dropped stream waits for its scheduled reconnect, which then starts it")
    func reconnect() {
        #expect(LogStreamPolicy.decide(current: nil, wanted: running, state: .ended("reset"), reconnectPending: true, isReconnect: false) == .wait)
        #expect(LogStreamPolicy.decide(current: nil, wanted: running, state: .ended("reset"), reconnectPending: true, isReconnect: true) == .start)
    }
}

@Suite("Pausing and the log stream")
struct PauseLogTests {
    @Test("resuming updates while following jumps back to the newest log line")
    func resume() {
        var state = AppState()
        state.selectedIDs[.containers] = "id-web-1"
        state.detailPane = .logs
        _ = state.handle(.key(KeyEvent(.character(" "))), snapshot: AppFixtures.snapshot(), hitMap: HitMap(), now: 0)
        #expect(state.isPaused)
        state.detailScroll = 40 // lines that arrived while paused, held off screen
        _ = state.handle(.key(KeyEvent(.character(" "))), snapshot: AppFixtures.snapshot(), hitMap: HitMap(), now: 0)
        #expect(!state.isPaused)
        #expect(state.detailScroll == 0)
        state.logFollow = false
        state.detailScroll = 12
        _ = state.handle(.key(KeyEvent(.character(" "))), snapshot: AppFixtures.snapshot(), hitMap: HitMap(), now: 0)
        _ = state.handle(.key(KeyEvent(.character(" "))), snapshot: AppFixtures.snapshot(), hitMap: HitMap(), now: 0)
        #expect(state.detailScroll == 12, "a scrolled-back view stays where the user put it")
    }

    @Test("the logs pane says when updates are paused")
    func indicator() {
        var state = AppState()
        state.selectedIDs[.containers] = "id-web-1"
        state.detailPane = .logs
        state.isPaused = true
        var snapshot = AppFixtures.snapshot()
        snapshot.detail = ContainerDetailData(containerID: "id-web-1", logs: [LogLine(stream: .stdout, text: "x")])
        snapshot.detail?.logState = .live
        let text = AppView.render(RenderContext(state: state, snapshot: snapshot, theme: .harbor,
                                                size: TerminalSize(columns: 150, rows: 40), now: 0)).buffer.plainText
        #expect(text.contains("⏸ updates paused"))
    }
}
