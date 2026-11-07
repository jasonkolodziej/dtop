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
    case dockerEvent(DockerEvent)
    case refreshed(DockerSnapshot)
    case detailLoaded(ContainerDetailData)
    case operationFinished(OperationResult)
    case terminate
}

/// Owns the terminal and the main loop. All UI state lives on the main actor; Docker I/O runs
/// in `DataRefresher` and background tasks that report back through one event stream.
@MainActor
public final class AppController {
    private let configuration: AppConfiguration
    private let terminal: Terminal
    private let refresher: DataRefresher
    private let api: any DockerAPI
    private let themeLoader: ThemeLoader
    private let themeNames: [String]
    private let readFile: @Sendable (String) -> String?

    private var state: AppState
    private var snapshot: DockerSnapshot
    private var theme: Theme
    private var renderer: FrameRenderer
    private var hitMap = HitMap()
    private var parser = InputParser()
    private var inputGeneration = 0
    private var size: TerminalSize

    private var isRefreshing = false
    private var pendingRefresh: Bool?
    private var eventRefreshDue = false
    private var tickCount = 0
    private var detailKey: String?
    private var isLoadingDetail = false
    private var lastDetailLoad = 0.0

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
        api: any DockerAPI,
        refresher: DataRefresher,
        theme: Theme,
        colorMode: ColorMode,
        themeLoader: ThemeLoader,
        themeNames: [String],
        readFile: @escaping @Sendable (String) -> String?
    ) {
        self.configuration = configuration
        self.terminal = terminal
        self.api = api
        self.refresher = refresher
        self.theme = theme
        self.themeLoader = themeLoader
        self.themeNames = themeNames
        self.readFile = readFile
        self.renderer = FrameRenderer(colorMode: colorMode)
        var state = AppState(configuration: configuration)
        state.themeName = theme.name
        self.state = state
        self.snapshot = DockerSnapshot(metrics: MetricsStore(capacity: configuration.historySize))
        self.size = terminal.size()
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss"
        self.clockFormatter = formatter
        (events, continuation) = AsyncStream.makeStream(of: LoopEvent.self, bufferingPolicy: .unbounded)
    }

    private func now() -> Double { Date().timeIntervalSince1970 }

    public func run() async throws {
        try terminal.enableRawMode()
        terminal.enterFullscreen(mouse: configuration.mouse)
        terminal.write(TerminalSequence.setTitle("dtop"))
        defer { shutdown() }

        snapshot = await refresher.snapshot
        startInputThread()
        startTimers()
        startDockerEvents()
        installSignalHandlers()
        requestRefresh(inventory: true)
        render()

        for await event in events {
            handle(event)
            if state.shouldQuit { break }
            render()
        }
    }

    private func shutdown() {
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

    private func startDockerEvents() {
        let api = self.api
        let continuation = self.continuation
        tasks.append(Task.detached {
            var backoff = Backoff(initialDelay: 1, maximumDelay: 30)
            while !Task.isCancelled {
                do {
                    for try await event in api.events() {
                        backoff.reset()
                        continuation.yield(.dockerEvent(event))
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
            if snapshot.connection.isConnected || snapshot.connection == .connecting {
                requestRefresh(inventory: state.tab.usesInventory || tickCount % 5 == 0)
            }
            loadDetailIfNeeded(force: true)
        case .fastTick:
            state.expireNotice(now: now)
            let current = terminal.size()
            if current != size {
                size = current
                renderer.invalidate()
            }
            if case .disconnected(_, let retryAt?) = snapshot.connection, now >= retryAt, !state.isPaused {
                requestRefresh(inventory: true)
            }
            if eventRefreshDue, !state.isPaused {
                eventRefreshDue = false
                requestRefresh(inventory: true)
            }
        case .dockerEvent(let dockerEvent):
            if dockerEvent.affectsInventory { eventRefreshDue = true }
        case .refreshed(let newSnapshot):
            isRefreshing = false
            let detail = snapshot.detail
            snapshot = newSnapshot
            if snapshot.detail == nil { snapshot.detail = detail }
            state.reconcileSelection(snapshot: snapshot)
            loadDetailIfNeeded(force: false)
            if let inventory = pendingRefresh {
                pendingRefresh = nil
                requestRefresh(inventory: inventory)
            }
        case .detailLoaded(let data):
            isLoadingDetail = false
            if ListModel.selectedItem(snapshot: snapshot, state: state)?.container?.id == data.containerID {
                snapshot.detail = data
            }
        case .operationFinished(let result):
            state.post(result.message, level: result.succeeded ? .success : .error, now: now,
                       duration: result.succeeded ? 4 : 8)
            requestRefresh(inventory: true)
            loadDetailIfNeeded(force: true)
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
    }

    private func execute(_ command: AppCommand) {
        switch command {
        case .quit:
            state.shouldQuit = true
        case .refresh(let inventory):
            requestRefresh(inventory: inventory)
        case .perform(let operation):
            let refresher = self.refresher
            let continuation = self.continuation
            tasks.append(Task.detached {
                let result = await refresher.perform(operation)
                continuation.yield(.operationFinished(result))
            })
        case .themeChanged(let name):
            do {
                theme = try themeLoader.load(name, readFile: readFile)
                renderer.invalidate()
            } catch {
                state.post(String(describing: error), level: .error, now: now(), duration: 6)
            }
        }
    }

    private func requestRefresh(inventory: Bool) {
        guard !isRefreshing else {
            pendingRefresh = (pendingRefresh ?? false) || inventory
            return
        }
        isRefreshing = true
        let refresher = self.refresher
        let continuation = self.continuation
        tasks.removeAll { $0.isCancelled }
        tasks.append(Task.detached {
            let snapshot = await refresher.refresh(includeInventory: inventory)
            continuation.yield(.refreshed(snapshot))
        })
    }

    /// Loads detail data for the selected container when the pane is visible.
    private func loadDetailIfNeeded(force: Bool) {
        guard state.showDetails,
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
        let refresher = self.refresher
        let continuation = self.continuation
        let pane = state.detailPane
        let isActive = container.state.isActive
        let id = container.id
        tasks.append(Task.detached {
            let data = await refresher.loadDetail(containerID: id, pane: pane, isActive: isActive)
            continuation.yield(.detailLoaded(data))
        })
    }

    // MARK: - Rendering

    private func render() {
        let context = RenderContext(
            state: state,
            snapshot: snapshot,
            theme: theme,
            size: size,
            clock: clockFormatter.string(from: Date()),
            now: now()
        )
        let frame = AppView.render(context)
        hitMap = frame.hitMap
        state.scrollOffsets[state.tab] = frame.hitMap.listScrollOffset
        terminal.write(renderer.render(frame.buffer))
    }
}
