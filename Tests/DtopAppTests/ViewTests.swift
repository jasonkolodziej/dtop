import Testing
@testable import DtopApp
@testable import DtopCore
@testable import DtopUI

@Suite("Screen layout")
struct ScreenLayoutTests {
    @Test("too-small terminals get no layout")
    func tooSmall() {
        #expect(ScreenLayout.compute(size: TerminalSize(columns: 39, rows: 30), showDetails: true) == nil)
        #expect(ScreenLayout.compute(size: TerminalSize(columns: 100, rows: 9), showDetails: true) == nil)
    }

    @Test("wide terminals put details beside the list, tall ones below")
    func placement() throws {
        let wide = try #require(ScreenLayout.compute(size: TerminalSize(columns: 160, rows: 40), showDetails: true))
        #expect(wide.detail?.y == wide.list.y)
        #expect(wide.list.maxX == wide.detail?.x)
        #expect(wide.metrics?.height == 10)

        let tall = try #require(ScreenLayout.compute(size: TerminalSize(columns: 100, rows: 50), showDetails: true))
        #expect(tall.detail?.x == tall.list.x)
        #expect(tall.list.maxY == tall.detail?.y)

        let short = try #require(ScreenLayout.compute(size: TerminalSize(columns: 100, rows: 20), showDetails: true))
        #expect(short.detail == nil)
        #expect(short.metrics == nil)

        let hidden = try #require(ScreenLayout.compute(size: TerminalSize(columns: 160, rows: 40), showDetails: false))
        #expect(hidden.detail == nil)
        #expect(hidden.list.width == 160)
    }

    @Test("regions tile the screen without overlap", arguments: [(80, 24), (120, 30), (200, 60), (40, 10)])
    func tiling(columns: Int, rows: Int) throws {
        let layout = try #require(ScreenLayout.compute(size: TerminalSize(columns: columns, rows: rows), showDetails: true))
        #expect(layout.header.y == 0)
        #expect(layout.footer.maxY == rows)
        let bodyBottom = max(layout.list.maxY, layout.detail?.maxY ?? 0)
        #expect(bodyBottom == layout.footer.y)
        #expect(layout.tabs.maxY == layout.list.y)
        if let metrics = layout.metrics { #expect(metrics.maxY == layout.tabs.y) }
    }
}

@Suite("App view rendering")
struct AppViewTests {
    func render(_ state: AppState = AppState(), snapshot: DockerSnapshot = AppFixtures.snapshot(),
                columns: Int = 140, rows: Int = 40, theme: Theme = .harbor) -> RenderedFrame {
        AppView.render(RenderContext(state: state, snapshot: snapshot, theme: theme,
                                     size: TerminalSize(columns: columns, rows: rows), clock: "12:34:56", now: 1_000))
    }

    @Test("header shows docker version, host facts, counts and the clock")
    func header() {
        let line = render().buffer.text(row: 0)
        #expect(line.contains("dtop"))
        #expect(line.contains("docker 27.0.0"))
        #expect(line.contains("4 cpu"))
        #expect(line.contains("● 2 up"))
        #expect(line.contains("◌ 2 down"))
        #expect(line.hasSuffix("12:34:56 "))
    }

    @Test("metrics boxes, tab bar with counts and hit rects")
    func tabsAndMetrics() {
        let frame = render()
        let text = frame.buffer.plainText
        #expect(text.contains("cpu"))
        #expect(text.contains("of 4 cpu"))
        #expect(text.contains("mem"))
        #expect(text.contains("net"))
        #expect(text.contains("1 Containers 4"))
        #expect(text.contains("2 Compose 1"))
        #expect(text.contains("3 Images 3"))
        #expect(frame.hitMap.tabs.map(\.tab) == Tab.allCases)
        for hit in frame.hitMap.tabs {
            #expect(frame.buffer.text(row: hit.rect.y).contains(hit.tab.title))
        }
    }

    @Test("container rows show state, metrics and hit rects in list order")
    func containerRows() {
        let frame = render()
        let lines = frame.buffer.lines
        let cacheRow = lines.firstIndex { $0.contains("cache") && $0.contains("redis") }
        #expect(cacheRow != nil)
        if let cacheRow {
            #expect(lines[cacheRow].contains("40.0"))
            #expect(lines[cacheRow].contains("9.5MiB"))
        }
        #expect(frame.hitMap.rows.map(\.index) == [0, 1, 2, 3])
        let oldRow = frame.hitMap.rows[3].rect.y
        #expect(lines[oldRow].contains("old"))
        #expect(lines[oldRow].contains("exited"))
        #expect(frame.hitMap.listVisibleRows == 4)
    }

    @Test("the selected row uses the theme's selection background")
    func selection() {
        var state = AppState()
        state.selectedIDs[.containers] = "id-web-1"
        let frame = render(state)
        let row = frame.hitMap.rows[1]
        #expect(frame.buffer[row.rect.x + 3, row.rect.y].style.background == Theme.harbor[.selectionBackground])
        #expect(frame.buffer[frame.hitMap.rows[0].rect.x + 3, frame.hitMap.rows[0].rect.y].style.background
            != Theme.harbor[.selectionBackground])
    }

    @Test("details panel shows container facts and pane tabs")
    func details() {
        var state = AppState()
        state.selectedIDs[.containers] = "id-web-1"
        let frame = render(state)
        let text = frame.buffer.plainText
        #expect(text.contains("Overview"))
        #expect(text.contains("Processes"))
        #expect(text.contains("Logs"))
        #expect(text.contains("shop / web"))
        #expect(frame.hitMap.detailPanes.map(\.pane) == DetailPane.allCases)
        #expect(!frame.hitMap.detailRect.isEmpty)
    }

    @Test("detail panes render loaded processes and logs")
    func detailPanes() {
        var snapshot = AppFixtures.snapshot()
        snapshot.detail = ContainerDetailData(
            containerID: "id-cache",
            processes: ProcessTree.build([
                ContainerProcess(pid: 1, parentPID: 0, cpu: "0", command: "redis-server"),
                ContainerProcess(pid: 9, parentPID: 1, cpu: "3", command: "bgsave")
            ]),
            logs: [LogLine(stream: .stdout, text: "Ready to accept connections"), LogLine(stream: .stderr, text: "WARNING overcommit")]
        )
        var state = AppState()
        state.detailPane = .processes
        let processes = render(state, snapshot: snapshot).buffer.plainText
        #expect(processes.contains("redis-server"))
        #expect(processes.contains("└─ bgsave"))
        state.detailPane = .logs
        let logFrame = render(state, snapshot: snapshot)
        #expect(logFrame.buffer.plainText.contains("Ready to accept connections"))
        let warningRow = logFrame.buffer.lines.firstIndex { $0.contains("WARNING overcommit") }!
        let column = logFrame.buffer.lines[warningRow].distance(
            from: logFrame.buffer.lines[warningRow].startIndex,
            to: logFrame.buffer.lines[warningRow].range(of: "WARNING")!.lowerBound)
        #expect(logFrame.buffer[column, warningRow].style.foreground == Theme.harbor[.warning], "stderr is highlighted")
    }

    @Test("panes show placeholders before data arrives or when stopped")
    func detailPlaceholders() {
        var state = AppState()
        state.detailPane = .processes
        #expect(render(state).buffer.plainText.contains("Loading processes"))
        state.selectedIDs[.containers] = "id-old"
        #expect(render(state).buffer.plainText.contains("Container is not running"))
        state.detailPane = .logs
        #expect(render(state).buffer.plainText.contains("Loading logs"))
        var snapshot = AppFixtures.snapshot()
        snapshot.detail = ContainerDetailData(containerID: "id-old", error: "No such container")
        #expect(render(state, snapshot: snapshot).buffer.plainText.contains("No such container"))
        snapshot.detail = ContainerDetailData(containerID: "id-old")
        #expect(render(state, snapshot: snapshot).buffer.plainText.contains("No log output"))
    }

    @Test("compose tab renders the project tree")
    func compose() {
        var state = AppState()
        state.tab = .compose
        let text = render(state).buffer.plainText
        #expect(text.contains("▾ shop"))
        #expect(text.contains("1/2 up"))
        #expect(text.contains("├─ db-1"))
        #expect(text.contains("└─ web-1"))
        #expect(text.contains("compose 1"))
        #expect(text.contains("directory"))
        #expect(text.contains("/srv/shop"))
    }

    @Test("inventory tabs render their rows and details", arguments: [
        (Tab.images, ["redis:7", "nginx:latest", "<none>", "used by"]),
        (.networks, ["shop_default", "bridge", "built-in network"]),
        (.volumes, ["shop_data", "(anonymous)"])
    ])
    func inventory(tab: Tab, expected: [String]) {
        var state = AppState()
        state.tab = tab
        if tab == .images { state.selectedIDs[.images] = "sha256:aaa" }
        if tab == .networks { state.selectedIDs[.networks] = "n1" }
        let text = render(state).buffer.plainText
        for fragment in expected {
            #expect(text.contains(fragment), "\(tab) should show \(fragment)")
        }
    }

    @Test("empty states explain what to do", arguments: [
        (ConnectionState.connecting, "Connecting to Docker"),
        (.disconnected(message: "Cannot connect to Docker", retryAt: 1_004), "Retrying in 4s"),
        (.connected, "No containers yet")
    ])
    func emptyStates(connection: ConnectionState, expected: String) {
        let snapshot = DockerSnapshot(endpoint: "unix:///x.sock", connection: connection)
        #expect(render(snapshot: snapshot).buffer.plainText.contains(expected))
    }

    @Test("filters that match nothing say how to clear them")
    func emptyFilter() {
        var state = AppState()
        state.filterText = "zzz"
        #expect(render(state).buffer.plainText.contains("press Esc to clear"))
        state.filterText = ""
        state.statusFilter = .running
        #expect(render(state, snapshot: DockerSnapshot(connection: .connected)).buffer.plainText.contains("press a to change"))
        state.tab = .compose
        #expect(render(state, snapshot: DockerSnapshot(connection: .connected)).buffer.plainText.contains("docker compose up"))
    }

    @Test("header reflects offline and paused states")
    func headerStates() {
        var snapshot = AppFixtures.snapshot()
        snapshot.connection = .disconnected(message: "gone", retryAt: 1_002.2)
        var state = AppState()
        state.isPaused = true
        let header = render(state, snapshot: snapshot).buffer.text(row: 0)
        #expect(header.contains("offline · retry 3s"))
        #expect(header.contains("paused"))
        snapshot.connection = .connecting
        #expect(render(snapshot: snapshot).buffer.text(row: 0).contains("connecting"))
    }

    @Test("footer shows hints, notices, filter input and connection errors")
    func footer() {
        let rows = 40
        var state = AppState()
        #expect(render(state).buffer.text(row: rows - 1).contains("q quit"))
        state.tab = .compose
        #expect(render(state).buffer.text(row: rows - 1).contains("U up"))
        state.notice = Notice(text: "Stopped web", level: .success, expiresAt: nil)
        #expect(render(state).buffer.text(row: rows - 1).contains("Stopped web"))
        state.notice = nil
        state.mode = .filter
        state.filterText = "abc"
        #expect(render(state).buffer.text(row: rows - 1).contains("/abc"))
        state.mode = .normal
        var snapshot = AppFixtures.snapshot()
        snapshot.connection = .disconnected(message: "Cannot connect to Docker at unix:///x", retryAt: nil)
        #expect(render(state, snapshot: snapshot).buffer.text(row: rows - 1).contains("Cannot connect"))
    }

    @Test("help overlay lists key bindings")
    func help() {
        var state = AppState()
        state.mode = .help
        let text = render(state).buffer.plainText
        #expect(text.contains("keys"))
        #expect(text.contains("Navigate"))
        #expect(text.contains("compose up"))
        #expect(text.contains("any key closes"))
    }

    @Test("confirmation dialog renders and exposes clickable buttons")
    func confirmation() throws {
        var state = AppState()
        state.mode = .confirm(Confirmation(operation: .removeVolume(name: "shop_data")))
        let frame = render(state)
        let text = frame.buffer.plainText
        #expect(text.contains("Remove volume?"))
        #expect(text.contains("permanently"))
        let yes = try #require(frame.hitMap.confirmButton)
        let no = try #require(frame.hitMap.cancelButton)
        #expect(frame.buffer.text(row: yes.y).contains("Yes (y)"))
        #expect(yes.maxX <= no.x)
        #expect(frame.buffer[no.x, no.y].style.background == Theme.harbor[.selectionBackground], "No is focused by default")
    }

    @Test("too-small terminals get a clear message")
    func tooSmall() {
        let text = render(columns: 30, rows: 8).buffer.plainText
        #expect(text.contains("Terminal too small"))
    }

    @Test("rendering never crashes across sizes, tabs, themes and modes")
    func robustness() {
        let snapshot = AppFixtures.snapshot()
        let modes: [InputMode] = [.normal, .filter, .help, .confirm(Confirmation(operation: .prune(.volumes)))]
        for (columns, rows) in [(40, 10), (60, 15), (80, 24), (100, 30), (133, 37), (220, 70)] {
            for tab in Tab.allCases {
                for theme in Theme.builtIn {
                    var state = AppState()
                    state.tab = tab
                    state.mode = modes[(columns + rows + tab.rawValue) % modes.count]
                    state.detailPane = DetailPane.allCases[(columns / 10) % DetailPane.allCases.count]
                    let frame = render(state, snapshot: snapshot, columns: columns, rows: rows, theme: theme)
                    #expect(frame.buffer.width == columns)
                    #expect(frame.buffer.height == rows)
                }
            }
        }
    }

    @Test("rendering into the diff renderer twice emits nothing the second time")
    func stableFrames() {
        let frame = render()
        var renderer = FrameRenderer(colorMode: .trueColor)
        #expect(!renderer.render(frame.buffer).isEmpty)
        #expect(renderer.render(render().buffer).isEmpty)
    }

    @Test("long lists scroll to keep the selection visible")
    func scrolling() {
        var snapshot = AppFixtures.snapshot()
        snapshot.containers = (0..<60).map { AppFixtures.container(String(format: "svc-%02d", $0)) }
        var state = AppState()
        state.sortKey = .name
        state.sortDescending = false
        state.selectedIDs[.containers] = "id-svc-50"
        let frame = render(state, snapshot: snapshot, columns: 100, rows: 30)
        #expect(frame.hitMap.rows.contains { $0.index == 50 })
        #expect(frame.hitMap.listScrollOffset > 0)
        #expect(frame.buffer.plainText.contains("svc-50"))
        #expect(!frame.buffer.plainText.contains("svc-00"))
    }

    @Test("state symbols and roles cover every state")
    func symbols() {
        let states: [ContainerState] = [.running, .paused, .restarting, .created, .dead, .removing, .exited, .unknown("x")]
        #expect(Set(states.map(AppView.stateSymbol)).count >= 6)
        #expect(AppView.stateRole(.running, health: .unhealthy) == .unhealthy)
        #expect(AppView.stateRole(.running, health: .healthy) == .statusRunning)
        for status in [ComposeStatus.running, .partial, .degraded, .paused, .stopped, .empty] {
            _ = AppView.composeRole(status)
        }
        #expect(ListView.percentCell(3.14159) == "3.1")
        #expect(ListView.percentCell(42.6) == "43")
        #expect(ListView.networkCell(2048, 0) == "2.0K 0")
    }
}

@Suite("Once report")
struct OnceReportTests {
    @Test("prints a header, totals and a stable table sorted by CPU")
    func report() {
        let text = OnceReport.render(snapshot: AppFixtures.snapshot(), width: 140)
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        #expect(lines[0].contains("unix:///var/run/docker.sock"))
        #expect(lines[0].contains("docker 27.0.0 (api 1.45)"))
        #expect(lines[1].contains("2 running, 2 stopped"))
        #expect(lines[3].hasPrefix("NAME"))
        #expect(lines[4].hasPrefix("cache"))
        #expect(lines[5].hasPrefix("web-1"))
        #expect(lines[5].contains("shop"))
        #expect(lines[6].contains("-"))
        #expect(!text.contains("\u{1B}"), "no escape codes")
        #expect(text.hasSuffix("\n"))
    }

    @Test("respects the width and reports empty hosts and errors")
    func edges() {
        let narrow = OnceReport.render(snapshot: AppFixtures.snapshot(), width: 40)
        for line in narrow.split(separator: "\n").dropFirst(2) {
            #expect(TextWidth.of(String(line)) <= 40)
        }
        #expect(OnceReport.render(snapshot: DockerSnapshot(connection: .connected)).contains("(no containers)"))
        let failed = OnceReport.render(snapshot: DockerSnapshot(connection: .disconnected(message: "nope", retryAt: nil)))
        #expect(failed == "dtop: nope\n")
    }
}
