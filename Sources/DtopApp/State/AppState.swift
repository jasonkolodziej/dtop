import DtopCore
import DtopUI

public enum Tab: Int, Sendable, CaseIterable, Hashable {
    case containers
    case compose
    case images
    case networks
    case volumes

    public var title: String {
        switch self {
        case .containers: return "Containers"
        case .compose: return "Compose"
        case .images: return "Images"
        case .networks: return "Networks"
        case .volumes: return "Volumes"
        }
    }

    public var shortcut: Character { Character(String(rawValue + 1)) }

    public func next() -> Tab { Tab(rawValue: (rawValue + 1) % Tab.allCases.count)! }
    public func previous() -> Tab { Tab(rawValue: (rawValue + Tab.allCases.count - 1) % Tab.allCases.count)! }

    /// Tabs whose lists come from the slower "inventory" refresh.
    public var usesInventory: Bool { self == .images || self == .networks || self == .volumes }
}

public enum SortKey: String, Sendable, CaseIterable, Equatable {
    case name
    case cpu
    case memory
    case status
    case created
    case image

    public func next() -> SortKey {
        let all = SortKey.allCases
        return all[(all.firstIndex(of: self)! + 1) % all.count]
    }

    /// Whether descending is the natural first direction (biggest consumers first).
    public var prefersDescending: Bool { self == .cpu || self == .memory || self == .created }
}

public enum StatusFilter: String, Sendable, CaseIterable, Equatable {
    case all
    case running
    case stopped

    public func next() -> StatusFilter {
        let all = StatusFilter.allCases
        return all[(all.firstIndex(of: self)! + 1) % all.count]
    }

    public func matches(_ state: ContainerState) -> Bool {
        switch self {
        case .all: return true
        case .running: return state.isActive
        case .stopped: return !state.isActive
        }
    }
}

public enum DetailPane: String, Sendable, CaseIterable, Equatable {
    case overview
    case processes
    case logs

    public var title: String {
        switch self {
        case .overview: return "Overview"
        case .processes: return "Processes"
        case .logs: return "Logs"
        }
    }

    public func next() -> DetailPane {
        let all = DetailPane.allCases
        return all[(all.firstIndex(of: self)! + 1) % all.count]
    }
}

public enum NoticeLevel: Sendable, Equatable {
    case info
    case success
    case warning
    case error
}

public struct Notice: Sendable, Equatable {
    public var text: String
    public var level: NoticeLevel
    public var expiresAt: Double?

    public init(text: String, level: NoticeLevel, expiresAt: Double?) {
        self.text = text
        self.level = level
        self.expiresAt = expiresAt
    }
}

public struct Confirmation: Sendable, Equatable {
    public var operation: DockerOperation
    /// Which button has keyboard focus; destructive dialogs default to "No".
    public var confirmFocused = false

    public init(operation: DockerOperation, confirmFocused: Bool = false) {
        self.operation = operation
        self.confirmFocused = confirmFocused
    }
}

public enum InputMode: Sendable, Equatable {
    case normal
    case filter
    case help
    case confirm(Confirmation)
    // Boxed: the editor carries several full themes, and keeping it inline made every AppState
    // copy ~600 bytes, which overflowed 512 KB secondary-thread stacks on macOS in debug builds.
    indirect case themeEditor(ThemeEditor)
}

/// All UI state. Pure value type: the reducer mutates it, views read it.
public struct AppState: Sendable, Equatable {
    public var tab: Tab = .containers
    /// Selected item ID per tab, so refreshes that reorder rows keep the same selection.
    public var selectedIDs: [Tab: String] = [:]
    /// Last selected index per tab, used when the selected item disappears.
    public var selectedIndexes: [Tab: Int] = [:]
    public var scrollOffsets: [Tab: Int] = [:]
    public var sortKey: SortKey = .cpu
    public var sortDescending = true
    public var filterText = ""
    public var statusFilter: StatusFilter = .all
    public var showDetails = true
    public var detailPane: DetailPane = .overview
    public var detailScroll = 0
    public var collapsedProjects: Set<String> = []
    public var mode: InputMode = .normal
    public var notice: Notice?
    public var themeName: String = "harbor"
    public var isPaused = false
    public var shouldQuit = false

    public init() {}

    public init(configuration: AppConfiguration) {
        sortKey = configuration.sortKey
        sortDescending = configuration.sortDescending ?? configuration.sortKey.prefersDescending
        statusFilter = configuration.showStopped ? .all : .running
        themeName = configuration.themeName
    }

    public mutating func post(_ text: String, level: NoticeLevel, now: Double, duration: Double? = 4) {
        notice = Notice(text: text, level: level, expiresAt: duration.map { now + $0 })
    }

    /// The editor, when the theme menu is open.
    public var themeEditor: ThemeEditor? {
        if case .themeEditor(let editor) = mode { return editor }
        return nil
    }

    public mutating func openThemeEditor(current: Theme, available: [Theme]) {
        mode = .themeEditor(ThemeEditor(current: current, available: available))
    }

    /// Called by the runtime after trying to persist a theme. Success closes the editor and makes
    /// the theme current; failure keeps the editor (and the unsaved draft) open with the reason.
    public mutating func finishThemeSave(name: String, error: String?, location: String?, now: Double) {
        guard case .themeEditor(var editor) = mode else { return }
        if let error {
            editor.field = .none
            editor.message = error
            mode = .themeEditor(editor)
            return
        }
        themeName = name
        mode = .normal
        post("Saved theme '\(name)'" + (location.map { " to \($0)" } ?? ""), level: .success, now: now, duration: 6)
    }

    public mutating func expireNotice(now: Double) {
        if let expiresAt = notice?.expiresAt, now >= expiresAt { notice = nil }
    }
}
