import DtopCore
import DtopUI

/// Side effects requested by the reducer; the runtime executes them.
public enum AppCommand: Sendable, Equatable {
    case quit
    case refresh(inventory: Bool)
    case perform(DockerOperation)
    case themeChanged(String)
    /// Open the theme menu; the runtime supplies the current and available themes.
    case openThemeEditor
    case saveTheme(name: String, theme: Theme)
    /// Show another host (index into `AppState.hostNames`).
    case switchHost(Int)
}

public struct HostHit: Sendable, Equatable {
    public var index: Int
    public var rect: Rect
}

public struct TabHit: Sendable, Equatable {
    public var tab: Tab
    public var rect: Rect
}

public struct RowHit: Sendable, Equatable {
    public var index: Int
    public var rect: Rect
}

public struct PaneHit: Sendable, Equatable {
    public var pane: DetailPane
    public var rect: Rect
}

/// Screen regions from the last frame, used to route mouse events.
public struct HitMap: Sendable, Equatable {
    public var tabs: [TabHit] = []
    public var rows: [RowHit] = []
    public var listRect: Rect = .zero
    public var listVisibleRows = 10
    public var listScrollOffset = 0
    public var detailRect: Rect = .zero
    public var detailPanes: [PaneHit] = []
    public var editorRows: [RowHit] = []
    public var confirmButton: Rect?
    public var cancelButton: Rect?
    /// Host chips in the header and rows of the host list.
    public var hostChips: [HostHit] = []
    public var hostRows: [HostHit] = []

    public init() {}
}

extension AppState {
    /// Applies one input event and returns the side effects to run.
    public mutating func handle(_ event: InputEvent, snapshot: DockerSnapshot, hitMap: HitMap, now: Double,
                                themeNames: [String] = Theme.builtIn.map(\.name)) -> [AppCommand] {
        if case .key(let key) = event, key.key == .character("c"), key.modifiers.contains(.ctrl) {
            shouldQuit = true
            return [.quit]
        }
        switch mode {
        case .help:
            if case .key = event { mode = .normal }
            if case .mouse(let mouse) = event, mouse.action == .press, !mouse.isWheel { mode = .normal }
            return []
        case .filter:
            return handleFilter(event)
        case .hostPicker(let selected):
            return handleHostPicker(event, selected: selected, hitMap: hitMap, now: now)
        case .confirm(let confirmation):
            return handleConfirm(event, confirmation: confirmation, hitMap: hitMap)
        case .themeEditor(var editor):
            switch editor.handle(event, rowHits: hitMap.editorRows) {
            case .editing:
                mode = .themeEditor(editor)
            case .cancelled:
                mode = .normal
                if editor.isDirty { post("Theme changes discarded", level: .info, now: now, duration: 3) }
            case .save(let name, let theme):
                mode = .themeEditor(editor)
                return [.saveTheme(name: name, theme: theme)]
            }
            return []
        case .normal:
            switch event {
            case .key(let key):
                return handleKey(key, snapshot: snapshot, hitMap: hitMap, now: now, themeNames: themeNames)
            case .mouse(let mouse):
                return handleMouse(mouse, snapshot: snapshot, hitMap: hitMap)
            case .paste(let text):
                mode = .filter
                return handleFilter(.paste(text))
            case .focus:
                return []
            }
        }
    }

    // MARK: - Normal mode

    private mutating func handleKey(_ event: KeyEvent, snapshot: DockerSnapshot, hitMap: HitMap, now: Double,
                                    themeNames: [String]) -> [AppCommand] {
        let items = ListModel.items(for: tab, snapshot: snapshot, state: self)
        let page = max(1, hitMap.listVisibleRows - 1)
        let ctrl = event.modifiers.contains(.ctrl)

        switch event.key {
        case .up: moveSelection(by: -1, items: items)
        case .down: moveSelection(by: 1, items: items)
        case .pageUp: moveSelection(by: -page, items: items)
        case .pageDown: moveSelection(by: page, items: items)
        case .home: moveSelection(to: 0, items: items)
        case .end: moveSelection(to: items.count - 1, items: items)
        case .tab, .right: switchTab(tab.next())
        case .backTab, .left: switchTab(tab.previous())
        case .function(1): mode = .help
        case .function(2): return [.openThemeEditor]
        case .function(5): return [.refresh(inventory: true)]
        case .escape:
            if !filterText.isEmpty {
                setFilter("")
            } else {
                notice = nil
            }
        case .enter:
            if case .composeProject(let project, _)? = selected(items) {
                toggleCollapsed(project.name)
            } else {
                showDetails.toggle()
            }
        case .character(let character) where ctrl:
            if character == "r" { return [.refresh(inventory: true)] }
            if character == "l" { return [.refresh(inventory: false)] }
        case .character(let character):
            return handleCharacter(character, items: items, snapshot: snapshot, now: now, themeNames: themeNames)
        default:
            break
        }
        return []
    }

    private mutating func handleCharacter(_ character: Character, items: [ListItem], snapshot: DockerSnapshot,
                                          now: Double, themeNames: [String]) -> [AppCommand] {
        switch character {
        case "q", "Q":
            shouldQuit = true
            return [.quit]
        case "?": mode = .help
        case "/": mode = .filter
        case "k": moveSelection(by: -1, items: items)
        case "j": moveSelection(by: 1, items: items)
        case "g": moveSelection(to: 0, items: items)
        case "G": moveSelection(to: items.count - 1, items: items)
        case "l": switchTab(tab.next())
        case "h": switchTab(tab.previous())
        case "1", "2", "3", "4", "5":
            if let index = Int(String(character)), let target = Tab(rawValue: index - 1) { switchTab(target) }
        case "s":
            sortKey = sortKey.next()
            sortDescending = sortKey.prefersDescending
            post("Sort: \(sortKey.rawValue) \(sortDescending ? "↓" : "↑")", level: .info, now: now, duration: 2)
        case "S":
            sortDescending.toggle()
            post("Sort: \(sortKey.rawValue) \(sortDescending ? "↓" : "↑")", level: .info, now: now, duration: 2)
        case "a":
            statusFilter = statusFilter.next()
            resetSelection()
            post("Showing: \(statusFilter.rawValue) containers", level: .info, now: now, duration: 2)
        case "d": showDetails.toggle()
        case "v":
            detailPane = detailPane.next()
            detailScroll = 0
            logFollow = true
            showDetails = true
        case "f":
            guard detailPane == .logs, showDetails else {
                post("f follows the log stream — open the Logs pane with v", level: .info, now: now, duration: 3)
                break
            }
            if logFollow {
                logFollow = false
                post("Log view paused — f to follow again", level: .info, now: now, duration: 2)
            } else {
                logFollow = true
                detailScroll = 0
            }
        case "c":
            if case .composeProject(let project, _)? = selected(items) { toggleCollapsed(project.name) }
        case "t":
            guard !themeNames.isEmpty else { break }
            let index = themeNames.firstIndex(of: themeName).map { ($0 + 1) % themeNames.count } ?? 0
            themeName = themeNames[index]
            post("Theme: \(themeName)", level: .info, now: now, duration: 2)
            return [.themeChanged(themeName)]
        case "T":
            return [.openThemeEditor]
        case " ":
            isPaused.toggle()
            post(isPaused ? "Updates paused — press space to resume" : "Updates resumed", level: .info, now: now,
                 duration: isPaused ? nil : 2)
        case "R":
            return [.refresh(inventory: true)]
        case "]", "[":
            guard hasSeveralHosts else {
                post(Self.singleHostHint, level: .info, now: now, duration: 4)
                break
            }
            let step = character == "]" ? 1 : hostNames.count - 1
            let target = (activeHost + step) % hostNames.count
            switchHost(to: target)
            post("Host: \(hostNames[target])", level: .info, now: now, duration: 2)
            return [.switchHost(target)]
        case "H":
            guard hasSeveralHosts else {
                post(Self.singleHostHint, level: .info, now: now, duration: 4)
                break
            }
            mode = .hostPicker(selected: activeHost)
        case "u", "x", "r", "p", "K", "D", "U", "X":
            return requestOperation(for: character, item: selected(items), now: now)
        default:
            break
        }
        return []
    }

    static let singleHostHint = "Only one Docker host — add more with --host, --context or --all-contexts"

    // MARK: - Host list

    private mutating func handleHostPicker(_ event: InputEvent, selected: Int, hitMap: HitMap, now: Double) -> [AppCommand] {
        let count = hostNames.count
        guard count > 0 else {
            mode = .normal
            return []
        }
        func choose(_ index: Int) -> [AppCommand] {
            mode = .normal
            guard index != activeHost else { return [] }
            switchHost(to: index)
            post("Host: \(hostNames[index])", level: .info, now: now, duration: 2)
            return [.switchHost(index)]
        }
        switch event {
        case .key(let key):
            switch key.key {
            case .up, .character("k"): mode = .hostPicker(selected: (selected + count - 1) % count)
            case .down, .character("j"), .tab: mode = .hostPicker(selected: (selected + 1) % count)
            case .home: mode = .hostPicker(selected: 0)
            case .end: mode = .hostPicker(selected: count - 1)
            case .enter, .character(" "): return choose(selected)
            case .escape, .character("q"), .character("H"): mode = .normal
            case .character(let character):
                if let digit = character.wholeNumberValue, (1...count).contains(digit) { return choose(digit - 1) }
            default: break
            }
        case .mouse(let mouse) where mouse.action == .press:
            if mouse.isWheel {
                let delta = mouse.button == .wheelUp ? count - 1 : 1
                mode = .hostPicker(selected: (selected + delta) % count)
            } else if let hit = hitMap.hostRows.first(where: { $0.rect.contains(x: mouse.x, y: mouse.y) }) {
                return choose(hit.index)
            } else {
                mode = .normal
            }
        default:
            break
        }
        return []
    }

    private mutating func requestOperation(for key: Character, item: ListItem?, now: Double) -> [AppCommand] {
        let operation: DockerOperation?
        if key == "X" {
            switch tab {
            case .containers, .compose: operation = .prune(.containers)
            case .images: operation = .prune(.images)
            case .networks: operation = .prune(.networks)
            case .volumes: operation = .prune(.volumes)
            }
        } else {
            guard let item else {
                post("Nothing selected", level: .warning, now: now)
                return []
            }
            operation = Self.operation(for: key, item: item)
            if operation == nil {
                post(Self.unavailableMessage(for: key, item: item), level: .warning, now: now)
                return []
            }
        }
        guard let operation else { return [] }
        if operation.isDestructive {
            mode = .confirm(Confirmation(operation: operation))
            return []
        }
        post(operation.progressDescription, level: .info, now: now, duration: nil)
        return [.perform(operation)]
    }

    /// Maps a key to an operation for the selected item, or nil when it does not apply.
    static func operation(for key: Character, item: ListItem) -> DockerOperation? {
        switch item {
        case .container(let container, _, _):
            let id = container.id
            let name = container.name
            switch key {
            case "u":
                if container.state == .paused { return .container(.unpause, id: id, name: name) }
                return container.state.isActive ? nil : .container(.start, id: id, name: name)
            case "x": return container.state.isActive ? .container(.stop(timeoutSeconds: nil), id: id, name: name) : nil
            case "r": return .container(.restart(timeoutSeconds: nil), id: id, name: name)
            case "p":
                if container.state == .running { return .container(.pause, id: id, name: name) }
                if container.state == .paused { return .container(.unpause, id: id, name: name) }
                return nil
            case "K": return container.state.isActive ? .container(.kill(signal: nil), id: id, name: name) : nil
            case "D": return .removeContainer(id: id, name: name, force: container.state.isActive)
            default: return nil
            }
        case .composeProject(let project, _):
            switch key {
            case "u": return .compose(.start, project: project.name)
            case "U": return .compose(.up, project: project.name)
            case "x": return .compose(.stop, project: project.name)
            case "r": return .compose(.restart, project: project.name)
            case "p":
                return .compose(project.aggregateStatus == .paused ? .unpause : .pause, project: project.name)
            case "D": return .compose(.down, project: project.name)
            default: return nil
            }
        case .image(let image):
            return key == "D" ? .removeImage(id: image.id, name: image.displayName, force: false) : nil
        case .network(let network):
            return key == "D" && !network.isBuiltIn ? .removeNetwork(id: network.id, name: network.name) : nil
        case .volume(let volume):
            return key == "D" ? .removeVolume(name: volume.name) : nil
        }
    }

    static func unavailableMessage(for key: Character, item: ListItem) -> String {
        switch item {
        case .container(let container, _, _):
            switch key {
            case "u": return "\(container.name) is already running"
            case "x", "K": return "\(container.name) is not running"
            case "p": return "\(container.name) can only be paused while running"
            default: return "That action does not apply to containers"
            }
        case .network(let network) where network.isBuiltIn && key == "D":
            return "\(network.name) is a built-in network and cannot be removed"
        default:
            return "That action does not apply here"
        }
    }

    private mutating func handleMouse(_ mouse: MouseEvent, snapshot: DockerSnapshot, hitMap: HitMap) -> [AppCommand] {
        let items = ListModel.items(for: tab, snapshot: snapshot, state: self)
        switch mouse.button {
        case .wheelUp, .wheelDown:
            let delta = mouse.button == .wheelUp ? -1 : 1
            if hitMap.detailRect.contains(x: mouse.x, y: mouse.y) {
                // The logs pane is anchored to the newest line and scrolls back in time, so the
                // wheel moves the other way there (wheel up = older lines).
                let showsLogs = detailPane == .logs && selected(items)?.container != nil
                detailScroll = max(0, detailScroll + (showsLogs ? -delta : delta) * 3)
                // Scrolling back pauses following; scrolling all the way down resumes it.
                if showsLogs { logFollow = detailScroll == 0 }
            } else {
                moveSelection(by: delta, items: items)
            }
        case .left where mouse.action == .press:
            if let hit = hitMap.hostChips.first(where: { $0.rect.contains(x: mouse.x, y: mouse.y) }) {
                guard hit.index != activeHost else { return [] }
                switchHost(to: hit.index)
                return [.switchHost(hit.index)]
            } else if let hit = hitMap.tabs.first(where: { $0.rect.contains(x: mouse.x, y: mouse.y) }) {
                switchTab(hit.tab)
            } else if let hit = hitMap.detailPanes.first(where: { $0.rect.contains(x: mouse.x, y: mouse.y) }) {
                detailPane = hit.pane
                detailScroll = 0
                logFollow = true
            } else if let hit = hitMap.rows.first(where: { $0.rect.contains(x: mouse.x, y: mouse.y) }), hit.index < items.count {
                let alreadySelected = ListModel.selectedIndex(in: items, state: self, tab: tab) == hit.index
                moveSelection(to: hit.index, items: items)
                if alreadySelected, case .composeProject(let project, _) = items[hit.index] {
                    toggleCollapsed(project.name)
                }
            }
        default:
            break
        }
        return []
    }

    // MARK: - Filter mode

    private mutating func handleFilter(_ event: InputEvent) -> [AppCommand] {
        switch event {
        case .key(let key):
            switch key.key {
            case .escape:
                setFilter("")
                mode = .normal
            case .enter, .tab, .down, .up:
                mode = .normal
            case .backspace:
                if !filterText.isEmpty { setFilter(String(filterText.dropLast())) }
            case .character(let character) where key.modifiers.contains(.ctrl):
                if character == "u" { setFilter("") }
                if character == "w" { setFilter(Self.deletingLastWord(filterText)) }
            case .character(let character) where !key.modifiers.contains(.alt):
                setFilter(filterText + String(character))
            default:
                break
            }
        case .paste(let text):
            let cleaned = String(text.filter { !$0.isNewline && $0 != "\t" })
            setFilter(filterText + cleaned)
        case .mouse(let mouse) where mouse.action == .press && !mouse.isWheel:
            mode = .normal
        default:
            break
        }
        return []
    }

    static func deletingLastWord(_ text: String) -> String {
        var trimmed = Substring(text)
        while trimmed.last == " " { trimmed = trimmed.dropLast() }
        while let last = trimmed.last, last != " " { trimmed = trimmed.dropLast() }
        return String(trimmed)
    }

    // MARK: - Confirm mode

    private mutating func handleConfirm(_ event: InputEvent, confirmation: Confirmation, hitMap: HitMap) -> [AppCommand] {
        var confirmation = confirmation
        switch event {
        case .key(let key):
            switch key.key {
            case .character("y"), .character("Y"):
                return confirm(confirmation.operation)
            case .character("n"), .character("N"), .character("q"), .escape:
                mode = .normal
            case .left, .right, .tab, .backTab, .character("h"), .character("l"):
                confirmation.confirmFocused.toggle()
                mode = .confirm(confirmation)
            case .enter:
                if confirmation.confirmFocused { return confirm(confirmation.operation) }
                mode = .normal
            default:
                break
            }
        case .mouse(let mouse) where mouse.action == .press && mouse.button == .left:
            if hitMap.confirmButton?.contains(x: mouse.x, y: mouse.y) == true {
                return confirm(confirmation.operation)
            }
            if hitMap.cancelButton?.contains(x: mouse.x, y: mouse.y) == true {
                mode = .normal
            }
        default:
            break
        }
        return []
    }

    private mutating func confirm(_ operation: DockerOperation) -> [AppCommand] {
        mode = .normal
        notice = Notice(text: operation.progressDescription, level: .info, expiresAt: nil)
        return [.perform(operation)]
    }

    // MARK: - Selection helpers

    func selected(_ items: [ListItem]) -> ListItem? {
        ListModel.selectedIndex(in: items, state: self, tab: tab).map { items[$0] }
    }

    mutating func moveSelection(by delta: Int, items: [ListItem]) {
        guard !items.isEmpty else { return }
        let current = ListModel.selectedIndex(in: items, state: self, tab: tab) ?? 0
        moveSelection(to: current + delta, items: items)
    }

    mutating func moveSelection(to index: Int, items: [ListItem]) {
        guard !items.isEmpty else { return }
        let clamped = min(max(0, index), items.count - 1)
        if selectedIDs[tab] != items[clamped].id { detailScroll = 0 }
        selectedIDs[tab] = items[clamped].id
        selectedIndexes[tab] = clamped
    }

    mutating func switchTab(_ target: Tab) {
        guard target != tab else { return }
        tab = target
        detailScroll = 0
    }

    mutating func toggleCollapsed(_ project: String) {
        if collapsedProjects.contains(project) {
            collapsedProjects.remove(project)
        } else {
            collapsedProjects.insert(project)
        }
    }

    mutating func setFilter(_ text: String) {
        filterText = text
        resetSelection()
    }

    mutating func resetSelection() {
        selectedIDs[tab] = nil
        selectedIndexes[tab] = 0
        scrollOffsets[tab] = 0
        detailScroll = 0
    }

    /// After a refresh, pins the selection to a row that still exists.
    public mutating func reconcileSelection(snapshot: DockerSnapshot) {
        for target in Tab.allCases {
            let rows = ListModel.items(for: target, snapshot: snapshot, state: self)
            guard let index = ListModel.selectedIndex(in: rows, state: self, tab: target) else {
                selectedIDs[target] = nil
                continue
            }
            selectedIDs[target] = rows[index].id
            selectedIndexes[target] = index
        }
    }
}
