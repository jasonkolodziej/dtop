import DtopCore
import DtopUI

public struct RenderContext: Sendable {
    public var state: AppState
    public var snapshot: DockerSnapshot
    public var theme: Theme
    public var size: TerminalSize
    /// Pre-formatted wall clock (formatting needs the local time zone, which views avoid).
    public var clock: String
    public var now: Double

    public init(state: AppState, snapshot: DockerSnapshot, theme: Theme, size: TerminalSize, clock: String = "", now: Double = 0) {
        self.state = state
        self.snapshot = snapshot
        self.theme = theme
        self.size = size
        self.clock = clock
        self.now = now
    }
}

public struct RenderedFrame: Sendable {
    public var buffer: ScreenBuffer
    public var hitMap: HitMap
}

/// Regions of the screen for a given size.
public struct ScreenLayout: Sendable, Equatable {
    public var header: Rect
    public var metrics: Rect?
    public var tabs: Rect
    public var list: Rect
    public var detail: Rect?
    public var footer: Rect

    public static let minimumSize = TerminalSize(columns: 40, rows: 10)

    public static func compute(size: TerminalSize, showDetails: Bool) -> ScreenLayout? {
        guard size.columns >= minimumSize.columns, size.rows >= minimumSize.rows else { return nil }
        let width = size.columns
        let height = size.rows
        let header = Rect(x: 0, y: 0, width: width, height: 1)
        let footer = Rect(x: 0, y: height - 1, width: width, height: 1)
        let metricsHeight = height >= 34 ? 10 : height >= 24 ? 7 : 0
        let metrics = metricsHeight > 0 ? Rect(x: 0, y: 1, width: width, height: metricsHeight) : nil
        let tabs = Rect(x: 0, y: 1 + metricsHeight, width: width, height: 1)
        let body = Rect(x: 0, y: tabs.maxY, width: width, height: footer.y - tabs.maxY)

        var list = body
        var detail: Rect?
        if showDetails {
            if width >= 120 {
                let parts = body.split(.horizontal, [.percent(58), .fill(1)])
                list = parts[0]
                detail = parts[1]
            } else if body.height >= 18 {
                let parts = body.split(.vertical, [.percent(55), .fill(1)])
                list = parts[0]
                detail = parts[1]
            }
        }
        return ScreenLayout(header: header, metrics: metrics, tabs: tabs, list: list, detail: detail, footer: footer)
    }
}

public enum AppView {
    public static func render(_ context: RenderContext) -> RenderedFrame {
        // While the theme menu is open, everything renders with its draft so edits show live.
        guard case .themeEditor(let editor) = context.state.mode else { return renderBody(context) }
        return renderBody(RenderContext(state: context.state, snapshot: context.snapshot, theme: editor.draft,
                                        size: context.size, clock: context.clock, now: context.now))
    }

    // Kept separate from `render` so debug builds don't reserve stack for several copies of the
    // large context (that pushed the frame to ~800 KB and overflowed 512 KB macOS thread stacks).
    private static func renderBody(_ context: RenderContext) -> RenderedFrame {
        let theme = context.theme
        var buffer = ScreenBuffer(
            width: context.size.columns,
            height: context.size.rows,
            fill: Cell(" ", style: theme.base)
        )
        var hitMap = HitMap()
        guard let layout = ScreenLayout.compute(size: context.size, showDetails: context.state.showDetails) else {
            let message = "Terminal too small (\(context.size.columns)x\(context.size.rows)); need "
                + "\(ScreenLayout.minimumSize.columns)x\(ScreenLayout.minimumSize.rows)"
            for (offset, line) in TextWidth.wrap(message, width: max(1, context.size.columns)).enumerated() {
                buffer.write(line, x: 0, y: offset, style: theme.style(.warning))
            }
            return RenderedFrame(buffer: buffer, hitMap: hitMap)
        }

        let items = ListModel.items(for: context.state.tab, snapshot: context.snapshot, state: context.state)
        let selectedIndex = ListModel.selectedIndex(in: items, state: context.state, tab: context.state.tab)

        HeaderView.draw(into: &buffer, rect: layout.header, context: context)
        if let metrics = layout.metrics {
            MetricsView.draw(into: &buffer, rect: metrics, context: context)
        }
        hitMap.tabs = TabBarView.draw(into: &buffer, rect: layout.tabs, context: context)
        let listLayout = ListView.draw(into: &buffer, rect: layout.list, items: items, selected: selectedIndex,
                                       context: context)
        hitMap.rows = listLayout.rowRects.map { RowHit(index: $0.index, rect: $0.rect) }
        hitMap.listRect = layout.list
        hitMap.listVisibleRows = max(1, listLayout.rowRects.count)
        hitMap.listScrollOffset = listLayout.scrollOffset
        if let detail = layout.detail {
            hitMap.detailRect = detail
            hitMap.detailPanes = DetailView.draw(into: &buffer, rect: detail,
                                                 item: selectedIndex.map { items[$0] }, context: context)
        }
        FooterView.draw(into: &buffer, rect: layout.footer, context: context)

        switch context.state.mode {
        case .help:
            OverlayView.drawHelp(into: &buffer, context: context)
        case .confirm(let confirmation):
            let buttons = OverlayView.drawConfirmation(into: &buffer, confirmation: confirmation, context: context)
            hitMap.confirmButton = buttons.confirm
            hitMap.cancelButton = buttons.cancel
        case .themeEditor(let editor):
            hitMap.editorRows = ThemeEditorView.draw(into: &buffer, editor: editor)
        default:
            break
        }
        return RenderedFrame(buffer: buffer, hitMap: hitMap)
    }

    // MARK: - Shared styling helpers

    static func stateRole(_ state: ContainerState, health: HealthStatus?) -> ThemeRole {
        if health == .unhealthy { return .unhealthy }
        switch state {
        case .running: return .statusRunning
        case .paused: return .statusPaused
        case .restarting: return .statusRestarting
        case .dead, .removing: return .statusDead
        case .created: return .statusCreated
        case .exited, .unknown: return .statusExited
        }
    }

    static func composeRole(_ status: ComposeStatus) -> ThemeRole {
        switch status {
        case .running: return .statusRunning
        case .partial: return .statusPaused
        case .degraded: return .statusDead
        case .paused: return .statusPaused
        case .stopped, .empty: return .statusExited
        }
    }

    static func stateSymbol(_ state: ContainerState) -> String {
        switch state {
        case .running: return "●"
        case .paused: return "◐"
        case .restarting: return "↻"
        case .created: return "○"
        case .dead, .removing: return "✕"
        case .exited, .unknown: return "◌"
        }
    }
}
