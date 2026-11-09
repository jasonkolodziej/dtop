#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

import Foundation
import DtopCore
import DtopUI

/// Everything that can wake the main loop.
enum LoopEvent: Sendable {
    case input([UInt8])
    case inputClosed
    case flushInput(generation: Int)
    case tick
    case fastTick
    case dockerEvent(host: Int, DockerEvent)
    case refreshed(host: Int, DockerSnapshot)
    case detailLoaded(host: Int, ContainerDetailData)
    case operationFinished(host: Int, OperationResult)
    case logLines(host: Int, containerID: String, [LogLine])
    case logStreamEnded(host: Int, containerID: String, error: String?)
    case terminate
}

/// One Docker host as the runtime sees it: where it is, how to talk to it, and its poller.
public struct HostConnection: Sendable {
    public var host: DockerHost
    public var api: any DockerAPI
    public var refresher: DataRefresher

    public init(host: DockerHost, api: any DockerAPI, refresher: DataRefresher) {
        self.host = host
        self.api = api
        self.refresher = refresher
    }
}

/// Owns the terminal and the main loop. All UI state lives on the main actor; Docker I/O runs
/// in one `DataRefresher` per host and background tasks that report back through one event
/// stream. Every host is polled; the views, details, log stream and operations follow the
/// active host (`state.activeHost`).
@MainActor
public final class AppController {
    /// Per-host runtime state.
    private struct Session {
        let connection: HostConnection
        var snapshot: DockerSnapshot
        var isRefreshing = false
        var pendingRefresh: Bool?
        var eventRefreshDue = false

        var refresher: DataRefresher { connection.refresher }
        var api: any DockerAPI { connection.api }
    }

    private let configuration: AppConfiguration
    private let terminal: Terminal
    private var sessions: [Session]
    private let themeLoader: ThemeLoader
    private var themeNames: [String]
    private let readFile: @Sendable (String) -> String?
    private let themeStore: ThemeStore
    private let fileSystem: ThemeFileSystem

    private var state: AppState
    private var theme: Theme
    private var renderer: FrameRenderer
    private var hitMap = HitMap()
    private var parser = InputParser()
    private var inputGeneration = 0
    private var size: TerminalSize

    private var tickCount = 0
    private var detailKey: String?
    private var isLoadingDetail = false
    private var lastDetailLoad = 0.0
    private var initialNotice: String?

    /// The followed log stream, when the logs pane is showing a container.
    private var logStream: (host: Int, containerID: String, task: Task<Void, Never>)?
    private var logRestartAt: Double?
    private let logTail: Int
    /// Lines that arrived while updates were paused (space); added when updates resume.
    private var pausedLogLines: [LogLine] = []

    private let events: AsyncStream<LoopEvent>
    private let continuation: AsyncStream<LoopEvent>.Continuation
    private var tasks: [Task<Void, Never>] = []
    #if !os(Windows)
    private var signalSources: [DispatchSourceSignal] = []
    #endif
    private let clockFormatter: DateFormatter

    public init(
        configuration: AppConfiguration,
        terminal: Terminal,
        hosts: [HostConnection],
        notice: String? = nil,
        logTail: Int = 300,
        theme: Theme,
        colorMode: ColorMode,
        themeLoader: ThemeLoader,
        themeNames: [String],
        readFile: @escaping @Sendable (String) -> String?,
        themeStore: ThemeStore,
        fileSystem: ThemeFileSystem = .local
    ) {
        self.themeStore = themeStore
        self.fileSystem = fileSystem
        self.configuration = configuration
        self.terminal = terminal
        self.sessions = hosts.map { connection in
            Session(connection: connection, snapshot: DockerSnapshot(endpoint: connection.host.description,
                                                                     metrics: MetricsStore(capacity: configuration.historySize)))
        }
        self.initialNotice = notice
        self.logTail = logTail
        self.theme = theme
        self.themeLoader = themeLoader
        self.themeNames = themeNames
        self.readFile = readFile
        self.renderer = FrameRenderer(colorMode: colorMode)
        var state = AppState(configuration: configuration)
        state.themeName = theme.name
        state.hostNames = hosts.map(\.host.name)
        self.state = state
        self.size = terminal.size()
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss"
        self.clockFormatter = formatter
        (events, continuation) = AsyncStream.makeStream(of: LoopEvent.self, bufferingPolicy: .unbounded)
    }

    private func now() -> Double { Date().timeIntervalSince1970 }

    private var active: Int { state.activeHost }

    /// The active host's snapshot, which the views and the reducer see.
    private var snapshot: DockerSnapshot {
        get { sessions[active].snapshot }
        set { sessions[active].snapshot = newValue }
    }

    public func run() async throws {
        try terminal.enableRawMode()
        terminal.enterFullscreen(mouse: configuration.mouse)
        terminal.write(TerminalSequence.setTitle("dtop"))
        defer { shutdown() }

        for index in sessions.indices {
            sessions[index].snapshot = await sessions[index].refresher.snapshot
        }
        startInputThread()
        startTimers()
        for index in sessions.indices { startDockerEvents(host: index) }
        installSignalHandlers()
        for index in sessions.indices { requestRefresh(host: index, inventory: true) }
        if let initialNotice {
            state.post(initialNotice, level: .warning, now: now(), duration: 10)
        }
        render()

        for await event in events {
            handle(event)
            if state.shouldQuit { break }
            render()
        }
    }

    private func shutdown() {
        logStream?.task.cancel()
        for task in tasks { task.cancel() }
        tasks.removeAll()
        continuation.finish()
        terminal.leaveFullscreen()
        terminal.restore()
    }

    // MARK: - Producers

    private func startInputThread() {
        let terminal = self.terminal
        let continuation = self.continuation
        let thread = Thread {
            while true {
                let bytes = terminal.read()
                if bytes.isEmpty {
                    continuation.yield(.inputClosed)
                    return
                }
                continuation.yield(.input(bytes))
            }
        }
        thread.name = "dtop.input"
        thread.start()
    }

    private func startTimers() {
        let continuation = self.continuation
        let interval = configuration.refreshInterval
        tasks.append(Task.detached {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
                continuation.yield(.tick)
            }
        })
        // A faster, cheap tick drives resize detection, debounced event refreshes and countdowns.
        tasks.append(Task.detached {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 250_000_000)
                continuation.yield(.fastTick)
            }
        })
    }

    private func startDockerEvents(host: Int) {
        let api = sessions[host].api
        let continuation = self.continuation
        tasks.append(Task.detached {
            var backoff = Backoff(initialDelay: 1, maximumDelay: 30)
            while !Task.isCancelled {
                do {
                    for try await event in api.events() {
                        backoff.reset()
                        continuation.yield(.dockerEvent(host: host, event))
                    }
                } catch {
                    // Connection problems are surfaced by the polling loop; just retry here.
                }
                let delay = backoff.nextDelay()
                try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            }
        })
    }

    private func installSignalHandlers() {
        #if !os(Windows)
        let continuation = self.continuation
        for signalNumber in [SIGTERM, SIGHUP, SIGINT] {
            signal(signalNumber, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: signalNumber, queue: .global())
            source.setEventHandler { continuation.yield(.terminate) }
            source.resume()
            signalSources.append(source)
        }
        #endif
    }

    // MARK: - Event handling

    private func handle(_ event: LoopEvent) {
        let now = self.now()
        switch event {
        case .input(let bytes):
            inputGeneration += 1
            for inputEvent in parser.feed(bytes) { apply(inputEvent, now: now) }
            if parser.hasPendingInput {
                // Give the rest of an escape sequence a moment to arrive before treating ESC as a
                // key; half-received sequences (mouse reports, pastes) get a longer grace period.
                let generation = inputGeneration
                let continuation = self.continuation
                let delay = UInt64(parser.flushDelay * 1_000_000_000)
                tasks.append(Task.detached {
                    try? await Task.sleep(nanoseconds: delay)
                    continuation.yield(.flushInput(generation: generation))
                })
            }
        case .flushInput(let generation):
            guard generation == inputGeneration else { return }
            for inputEvent in parser.flush() { apply(inputEvent, now: now) }
        case .inputClosed:
            state.shouldQuit = true
        case .terminate:
            state.shouldQuit = true
        case .tick:
            tickCount += 1
            guard !state.isPaused else { return }
            for index in sessions.indices {
                let connection = sessions[index].snapshot.connection
                guard connection.isConnected || connection == .connecting else { continue }
                // Background hosts only need their container list; inventory every fifth tick.
                let inventory = tickCount % 5 == 0 || (index == active && state.tab.usesInventory)
                requestRefresh(host: index, inventory: inventory)
            }
            loadDetailIfNeeded(force: true)
        case .fastTick:
            state.expireNotice(now: now)
            let current = terminal.size()
            if current != size {
                size = current
                renderer.invalidate()
            }
            guard !state.isPaused else { return }
            for index in sessions.indices {
                if case .disconnected(_, let retryAt?) = sessions[index].snapshot.connection, now >= retryAt {
                    requestRefresh(host: index, inventory: true)
                }
                if sessions[index].eventRefreshDue {
                    sessions[index].eventRefreshDue = false
                    requestRefresh(host: index, inventory: true)
                }
            }
            if let restartAt = logRestartAt, now >= restartAt {
                logRestartAt = nil
                updateLogStream(restart: true)
            }
        case .dockerEvent(let host, let dockerEvent):
            if dockerEvent.affectsInventory, sessions.indices.contains(host) { sessions[host].eventRefreshDue = true }
        case .refreshed(let host, var newSnapshot):
            guard sessions.indices.contains(host) else { return }
            sessions[host].isRefreshing = false
            // The controller owns the detail pane (it merges streamed logs into it).
            newSnapshot.detail = sessions[host].snapshot.detail
            sessions[host].snapshot = newSnapshot
            if host == active {
                state.reconcileSelection(snapshot: snapshot)
                loadDetailIfNeeded(force: false)
            }
            if let inventory = sessions[host].pendingRefresh {
                sessions[host].pendingRefresh = nil
                requestRefresh(host: host, inventory: inventory)
            }
        case .detailLoaded(let host, var data):
            guard host == active else { return }
            isLoadingDetail = false
            if ListModel.selectedItem(snapshot: snapshot, state: state)?.container?.id == data.containerID {
                if let current = snapshot.detail, current.containerID == data.containerID {
                    // Keep the followed log stream's lines and status.
                    data.logs = current.logs
                    data.logState = current.logState
                }
                snapshot.detail = data
            }
        case .operationFinished(let host, let result):
            state.post(result.message, level: result.succeeded ? .success : .error, now: now,
                       duration: result.succeeded ? 4 : 8)
            requestRefresh(host: host, inventory: true)
            loadDetailIfNeeded(force: true)
        case .logLines(let host, let containerID, let lines):
            guard let stream = logStream, stream.host == host, stream.containerID == containerID, host == active,
                  snapshot.detail?.containerID == containerID else { return }
            if state.isPaused {
                // Paused: the screen stays frozen; keep the lines for when updates resume.
                pausedLogLines += lines
                if pausedLogLines.count > ContainerDetailData.maximumLogLines {
                    pausedLogLines.removeFirst(pausedLogLines.count - ContainerDetailData.maximumLogLines)
                }
                return
            }
            appendStreamedLogs(lines)
        case .logStreamEnded(let host, let containerID, let error):
            guard let stream = logStream, stream.host == host, stream.containerID == containerID else { return }
            logStream = nil
            if host == active, var detail = snapshot.detail, detail.containerID == containerID {
                detail.logState = .ended(error)
                snapshot.detail = detail
            }
            // A running container's stream only ends on a hiccup: reconnect shortly.
            if host == active, snapshot.container(id: containerID)?.state.isActive == true {
                logRestartAt = now + (error == nil ? 1 : 3)
            }
        }
    }

    /// Adds streamed lines to the detail pane. When scrolled back, the same lines stay on screen.
    private func appendStreamedLogs(_ lines: [LogLine]) {
        guard var detail = snapshot.detail, !lines.isEmpty else { return }
        detail.appendLogs(lines)
        detail.logState = .live
        snapshot.detail = detail
        // Keep the same lines on screen while scrolled back; never past the oldest kept line
        // (the 5,000-line cap drops lines from the top).
        if !state.logFollow {
            state.detailScroll = LogStreamPolicy.scrollBack(state.detailScroll, appended: lines.count, keptLines: detail.logs.count)
        }
    }

    private func apply(_ event: InputEvent, now: Double) {
        let previousSelection = ListModel.selectedItem(snapshot: snapshot, state: state)?.id
        let previousPane = state.detailPane
        let commands = state.handle(event, snapshot: snapshot, hitMap: hitMap, now: now, themeNames: themeNames)
        for command in commands {
            execute(command)
        }
        let selection = ListModel.selectedItem(snapshot: snapshot, state: state)?.id
        if selection != previousSelection || state.detailPane != previousPane {
            loadDetailIfNeeded(force: false)
        }
        updateLogStream(restart: false)
        if !state.isPaused && !pausedLogLines.isEmpty {
            let lines = pausedLogLines
            pausedLogLines.removeAll()
            appendStreamedLogs(lines)
        }
    }

    private func execute(_ command: AppCommand) {
        switch command {
        case .quit:
            state.shouldQuit = true
        case .refresh(let inventory):
            requestRefresh(host: active, inventory: inventory)
        case .perform(let operation):
            let host = active
            let refresher = sessions[host].refresher
            let continuation = self.continuation
            tasks.append(Task.detached {
                let result = await refresher.perform(operation)
                continuation.yield(.operationFinished(host: host, result))
            })
        case .switchHost:
            // The reducer already moved `state.activeHost`; show that host's data.
            detailKey = nil
            isLoadingDetail = false
            state.reconcileSelection(snapshot: snapshot)
            if !snapshot.connection.isConnected || snapshot.lastUpdated == nil {
                requestRefresh(host: active, inventory: true)
            } else if state.tab.usesInventory {
                requestRefresh(host: active, inventory: true)
            }
            loadDetailIfNeeded(force: false)
        case .openThemeEditor:
            // Custom themes that fail to parse are skipped here; `t` reports their errors.
            var themes = themeNames.compactMap { try? themeLoader.load($0, readFile: readFile) }
            if themes.isEmpty { themes = Theme.builtIn }
            state.openThemeEditor(current: theme, available: themes)
        case .saveTheme(let name, let saved):
            do {
                let result = try themeStore.save(saved, name: name, fileSystem: fileSystem)
                theme = saved
                if !themeNames.contains(name) { themeNames.append(name) }
                state.finishThemeSave(name: name, error: nil, location: result.themeFile ?? result.configFile, now: now())
            } catch {
                state.finishThemeSave(name: name, error: String(describing: error), location: nil, now: now())
            }
        case .themeChanged(let name):
            do {
                theme = try themeLoader.load(name, readFile: readFile)
                renderer.invalidate()
            } catch {
                state.post(String(describing: error), level: .error, now: now(), duration: 6)
            }
        }
    }

    private func requestRefresh(host: Int, inventory: Bool) {
        guard sessions.indices.contains(host) else { return }
        guard !sessions[host].isRefreshing else {
            sessions[host].pendingRefresh = (sessions[host].pendingRefresh ?? false) || inventory
            return
        }
        sessions[host].isRefreshing = true
        let refresher = sessions[host].refresher
        let continuation = self.continuation
        tasks.removeAll { $0.isCancelled }
        tasks.append(Task.detached {
            let snapshot = await refresher.refresh(includeInventory: inventory)
            continuation.yield(.refreshed(host: host, snapshot))
        })
    }

    /// Loads detail data for the selected container when the pane is visible. The logs pane
    /// is fed by the followed stream instead (`updateLogStream`).
    private func loadDetailIfNeeded(force: Bool) {
        updateLogStream(restart: false)
        guard state.showDetails, state.detailPane != .logs,
              ScreenLayout.compute(size: size, showDetails: true)?.detail != nil,
              let container = ListModel.selectedItem(snapshot: snapshot, state: state)?.container else {
            return
        }
        let key = "\(container.id):\(state.detailPane.rawValue)"
        let now = self.now()
        // Overview data (inspect) changes rarely; processes and logs refresh with every tick.
        if force, key == detailKey, state.detailPane == .overview, now - lastDetailLoad < 10 { return }
        guard force || key != detailKey, !isLoadingDetail else { return }
        detailKey = key
        isLoadingDetail = true
        lastDetailLoad = now
        let host = active
        let refresher = sessions[host].refresher
        let continuation = self.continuation
        let pane = state.detailPane
        let isActive = container.state.isActive
        let id = container.id
        tasks.append(Task.detached {
            let data = await refresher.loadDetail(containerID: id, pane: pane, isActive: isActive)
            continuation.yield(.detailLoaded(host: host, data))
        })
    }

    /// Which container's logs should be streaming right now, if any.
    private func wantedLogStream() -> LogStreamPolicy.Target? {
        guard state.showDetails, state.detailPane == .logs,
              ScreenLayout.compute(size: size, showDetails: true)?.detail != nil,
              let container = ListModel.selectedItem(snapshot: snapshot, state: state)?.container else { return nil }
        return LogStreamPolicy.Target(host: active, containerID: container.id, isActive: container.state.isActive)
    }

    /// Starts, keeps or stops the followed log stream to match the selection (`LogStreamPolicy`).
    /// `restart` is a scheduled reconnect for a running container whose stream dropped.
    private func updateLogStream(restart: Bool) {
        let wanted = wantedLogStream()
        let existing = wanted.flatMap { target in snapshot.detail?.containerID == target.containerID ? snapshot.detail : nil }
        let decision = LogStreamPolicy.decide(current: logStream.map { ($0.host, $0.containerID) }, wanted: wanted,
                                              state: existing?.logState, reconnectPending: logRestartAt != nil,
                                              isReconnect: restart)
        switch decision {
        case .keep, .wait:
            return
        case .stop:
            logStream?.task.cancel()
            logStream = nil
            logRestartAt = nil
            return
        case .start:
            logStream?.task.cancel()
            logStream = nil
            logRestartAt = nil
        }
        guard let wanted else { return }

        pausedLogLines.removeAll()
        var detail = existing ?? ContainerDetailData(containerID: wanted.containerID)
        // Same container as before (a reconnect, or it started again): keep what is on screen
        // and resume from the last line's timestamp instead of re-reading the tail.
        let resumeAfter = detail.logs.last?.timestamp
        if resumeAfter == nil {
            detail.logs = []
            state.detailScroll = 0
            state.logFollow = true
        }
        detail.logState = .live
        snapshot.detail = detail

        let api = sessions[wanted.host].api
        let continuation = self.continuation
        let tail = logTail
        let host = wanted.host
        let id = wanted.containerID
        let task = Task.detached {
            do {
                for try await batch in api.followLogs(id: id, tail: tail, since: resumeAfter) {
                    // `since` is inclusive: drop lines already shown.
                    let fresh = LogStreamPolicy.newLines(batch, after: resumeAfter)
                    if !fresh.isEmpty { continuation.yield(.logLines(host: host, containerID: id, fresh)) }
                }
                continuation.yield(.logStreamEnded(host: host, containerID: id, error: nil))
            } catch {
                if !Task.isCancelled {
                    continuation.yield(.logStreamEnded(host: host, containerID: id, error: DataRefresher.describe(error)))
                }
            }
        }
        logStream = (wanted.host, wanted.containerID, task)
    }

    // MARK: - Rendering

    private func render() {
        let context = RenderContext(
            state: state,
            snapshot: snapshot,
            theme: theme,
            size: size,
            clock: clockFormatter.string(from: Date()),
            now: now(),
            hosts: sessions.map { HostSummary(name: $0.connection.host.name, description: $0.connection.host.description,
                                              snapshot: $0.snapshot) }
        )
        let frame = AppView.render(context)
        hitMap = frame.hitMap
        state.scrollOffsets[state.tab] = frame.hitMap.listScrollOffset
        terminal.write(renderer.render(frame.buffer))
    }
}
