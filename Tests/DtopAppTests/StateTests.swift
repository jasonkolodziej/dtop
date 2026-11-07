import Testing
@testable import DtopApp
@testable import DtopCore
@testable import DtopUI

@Suite("List model")
struct ListModelTests {
    let snapshot = AppFixtures.snapshot()

    func names(_ items: [ListItem]) -> [String] {
        items.map { item in
            switch item {
            case .container(let container, _, _): return container.name
            case .composeProject(let project, _): return "[\(project.name)]"
            case .image(let image): return image.displayName
            case .network(let network): return network.name
            case .volume(let volume): return volume.name
            }
        }
    }

    @Test("containers sort by CPU descending by default, idle ones by state then name")
    func defaultSort() {
        let items = ListModel.items(for: .containers, snapshot: snapshot, state: AppState())
        #expect(names(items) == ["cache", "web-1", "db-1", "old"])
    }

    @Test("each sort key orders containers", arguments: [
        (SortKey.name, false, ["cache", "db-1", "old", "web-1"]),
        (.name, true, ["web-1", "old", "db-1", "cache"]),
        (.memory, true, ["web-1", "cache", "db-1", "old"]),
        (.created, true, ["web-1", "db-1", "cache", "old"]),
        (.created, false, ["old", "cache", "db-1", "web-1"]),
        (.status, false, ["cache", "web-1", "db-1", "old"]),
        (.image, false, ["old", "web-1", "db-1", "cache"])
    ])
    func sorting(key: SortKey, descending: Bool, expected: [String]) {
        var state = AppState()
        state.sortKey = key
        state.sortDescending = descending
        #expect(names(ListModel.items(for: .containers, snapshot: snapshot, state: state)) == expected)
    }

    @Test("status filter and text filter combine")
    func filters() {
        var state = AppState()
        state.statusFilter = .running
        #expect(names(ListModel.items(for: .containers, snapshot: snapshot, state: state)) == ["cache", "web-1"])
        state.statusFilter = .stopped
        #expect(names(ListModel.items(for: .containers, snapshot: snapshot, state: state)) == ["db-1", "old"])
        state.statusFilter = .all
        state.filterText = "  SHOP "
        #expect(Set(names(ListModel.items(for: .containers, snapshot: snapshot, state: state))) == ["web-1", "db-1"])
        state.filterText = "redis"
        #expect(names(ListModel.items(for: .containers, snapshot: snapshot, state: state)) == ["cache"])
        state.filterText = "id-ol"
        #expect(names(ListModel.items(for: .containers, snapshot: snapshot, state: state)) == ["old"])
        state.filterText = "exited"
        #expect(Set(names(ListModel.items(for: .containers, snapshot: snapshot, state: state))) == ["db-1", "old"])
        state.filterText = "nothing-matches"
        #expect(ListModel.items(for: .containers, snapshot: snapshot, state: state).isEmpty)
    }

    @Test("compose tab nests containers under projects and honours collapse")
    func compose() {
        var state = AppState()
        let items = ListModel.items(for: .compose, snapshot: snapshot, state: state)
        #expect(names(items) == ["[shop]", "db-1", "web-1"])
        if case .container(_, let depth, let isLast) = items[2] {
            #expect(depth == 1)
            #expect(isLast)
        } else {
            Issue.record("expected a container row")
        }
        state.collapsedProjects = ["shop"]
        let collapsed = ListModel.items(for: .compose, snapshot: snapshot, state: state)
        #expect(names(collapsed) == ["[shop]"])
        if case .composeProject(_, let isCollapsed) = collapsed[0] { #expect(isCollapsed) }
    }

    @Test("compose filtering matches project names or member containers")
    func composeFilter() {
        var state = AppState()
        state.filterText = "web"
        #expect(names(ListModel.items(for: .compose, snapshot: snapshot, state: state)) == ["[shop]", "web-1"])
        state.filterText = "shop"
        #expect(names(ListModel.items(for: .compose, snapshot: snapshot, state: state)) == ["[shop]", "db-1", "web-1"])
        state.filterText = "cache"
        #expect(ListModel.items(for: .compose, snapshot: snapshot, state: state).isEmpty)
        state.filterText = ""
        state.statusFilter = .running
        #expect(names(ListModel.items(for: .compose, snapshot: snapshot, state: state)) == ["[shop]", "web-1"])
    }

    @Test("images, networks and volumes sort and filter")
    func inventory() {
        var state = AppState()
        state.sortKey = .name
        #expect(names(ListModel.items(for: .images, snapshot: snapshot, state: state)) == ["<none>", "nginx:latest", "redis:7"])
        state.sortKey = .created
        state.sortDescending = true
        #expect(names(ListModel.items(for: .images, snapshot: snapshot, state: state)) == ["nginx:latest", "redis:7", "<none>"])
        state.sortKey = .memory
        #expect(names(ListModel.items(for: .images, snapshot: snapshot, state: state)) == ["<none>", "redis:7", "nginx:latest"])
        state.sortDescending = false
        #expect(names(ListModel.items(for: .images, snapshot: snapshot, state: state)).first == "nginx:latest")
        state.filterText = "redis"
        #expect(names(ListModel.items(for: .images, snapshot: snapshot, state: state)) == ["redis:7"])
        state.filterText = "ccc"
        #expect(names(ListModel.items(for: .images, snapshot: snapshot, state: state)) == ["<none>"])

        state.filterText = ""
        #expect(names(ListModel.items(for: .networks, snapshot: snapshot, state: state)) == ["bridge", "shop_default"])
        let volumes = names(ListModel.items(for: .volumes, snapshot: snapshot, state: state))
        #expect(volumes.first == "shop_data", "named volumes come before anonymous ones")
        state.filterText = "shop"
        #expect(names(ListModel.items(for: .volumes, snapshot: snapshot, state: state)) == ["shop_data"])
    }

    @Test("item IDs are unique across kinds")
    func ids() {
        let project = ComposeProject(name: "x", services: [])
        #expect(ListItem.composeProject(project, collapsed: false).id == "project:x")
        #expect(ListItem.volume(DockerVolume(name: "x")).id == "volume:x")
        #expect(ListItem.composeProject(project, collapsed: false).container == nil)
        #expect(ListItem.composeProject(project, collapsed: false).composeProject == project)
    }

    @Test("selection follows the remembered ID, then the remembered index")
    func selection() {
        var state = AppState()
        let items = ListModel.items(for: .containers, snapshot: snapshot, state: state)
        #expect(ListModel.selectedIndex(in: items, state: state, tab: .containers) == 0)
        state.selectedIDs[.containers] = "id-old"
        #expect(ListModel.selectedIndex(in: items, state: state, tab: .containers) == 3)
        state.selectedIDs[.containers] = "id-gone"
        state.selectedIndexes[.containers] = 99
        #expect(ListModel.selectedIndex(in: items, state: state, tab: .containers) == 3)
        #expect(ListModel.selectedIndex(in: [], state: state, tab: .containers) == nil)
    }
}

@Suite("Reducer: navigation")
struct ReducerNavigationTests {
    let snapshot = AppFixtures.snapshot()

    func key(_ key: Key, _ modifiers: KeyModifiers = []) -> InputEvent { .key(KeyEvent(key, modifiers: modifiers)) }
    func char(_ character: Character) -> InputEvent { key(.character(character)) }

    func selectedName(_ state: AppState) -> String? {
        ListModel.selectedItem(snapshot: snapshot, state: state)?.container?.name
    }

    @Test("arrow keys, vim keys, paging and jumps move the selection within bounds")
    func movement() {
        var state = AppState()
        var hitMap = HitMap()
        hitMap.listVisibleRows = 3
        _ = state.handle(key(.down), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "web-1")
        _ = state.handle(char("j"), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "db-1")
        _ = state.handle(char("k"), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "web-1")
        _ = state.handle(char("G"), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "old")
        _ = state.handle(key(.down), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "old", "stays on the last row")
        _ = state.handle(char("g"), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "cache")
        _ = state.handle(key(.up), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "cache", "stays on the first row")
        _ = state.handle(key(.pageDown), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "db-1")
        _ = state.handle(key(.pageUp), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "cache")
        _ = state.handle(key(.end), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "old")
        _ = state.handle(key(.home), snapshot: snapshot, hitMap: hitMap, now: 0)
        #expect(selectedName(state) == "cache")
    }

    @Test("moving on an empty list is harmless")
    func emptyList() {
        var state = AppState()
        _ = state.handle(key(.down), snapshot: DockerSnapshot(), hitMap: HitMap(), now: 0)
        #expect(state.selectedIDs.isEmpty)
    }

    @Test("tabs cycle with Tab/arrows/h/l and jump with digits")
    func tabs() {
        var state = AppState()
        _ = state.handle(key(.tab), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.tab == .compose)
        _ = state.handle(key(.right), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.tab == .images)
        _ = state.handle(char("l"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.tab == .networks)
        _ = state.handle(char("h"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(key(.left), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(key(.backTab, .shift), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.tab == .containers)
        _ = state.handle(key(.left), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.tab == .volumes, "wraps around")
        _ = state.handle(char("3"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.tab == .images)
        _ = state.handle(char("9"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.tab == .images)
    }

    @Test("each tab keeps its own selection")
    func perTabSelection() {
        var state = AppState()
        _ = state.handle(char("j"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(char("2"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(char("j"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(char("j"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(char("1"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(selectedName(state) == "web-1")
        #expect(state.selectedIDs[.compose] == "id-web-1")
    }

    @Test("sort, status filter, details, pane, pause and theme toggles")
    func toggles() {
        var state = AppState()
        _ = state.handle(char("s"), snapshot: snapshot, hitMap: HitMap(), now: 10)
        #expect(state.sortKey == .memory)
        #expect(state.sortDescending)
        #expect(state.notice?.text.contains("memory") == true)
        _ = state.handle(char("s"), snapshot: snapshot, hitMap: HitMap(), now: 10)
        #expect(state.sortKey == .status)
        #expect(!state.sortDescending)
        _ = state.handle(char("S"), snapshot: snapshot, hitMap: HitMap(), now: 10)
        #expect(state.sortDescending)
        _ = state.handle(char("a"), snapshot: snapshot, hitMap: HitMap(), now: 10)
        #expect(state.statusFilter == .running)
        _ = state.handle(char("d"), snapshot: snapshot, hitMap: HitMap(), now: 10)
        #expect(!state.showDetails)
        _ = state.handle(char("v"), snapshot: snapshot, hitMap: HitMap(), now: 10)
        #expect(state.detailPane == .processes)
        #expect(state.showDetails, "cycling panes reopens details")
        _ = state.handle(char(" "), snapshot: snapshot, hitMap: HitMap(), now: 10)
        #expect(state.isPaused)
        #expect(state.notice?.expiresAt == nil, "the paused notice persists")
        _ = state.handle(char(" "), snapshot: snapshot, hitMap: HitMap(), now: 10)
        #expect(!state.isPaused)
        let commands = state.handle(char("t"), snapshot: snapshot, hitMap: HitMap(), now: 10, themeNames: ["harbor", "ansi"])
        #expect(commands == [.themeChanged("ansi")])
        #expect(state.handle(char("t"), snapshot: snapshot, hitMap: HitMap(), now: 10, themeNames: ["harbor", "ansi"])
            == [.themeChanged("harbor")])
        state.themeName = "custom-from-file"
        #expect(state.handle(char("t"), snapshot: snapshot, hitMap: HitMap(), now: 10, themeNames: ["harbor", "ansi"])
            == [.themeChanged("harbor")])
        #expect(state.handle(char("t"), snapshot: snapshot, hitMap: HitMap(), now: 10, themeNames: []).isEmpty)
    }

    @Test("quit, refresh and help keys")
    func commands() {
        var state = AppState()
        #expect(state.handle(char("R"), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.refresh(inventory: true)])
        #expect(state.handle(key(.function(5)), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.refresh(inventory: true)])
        #expect(state.handle(key(.character("r"), .ctrl), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.refresh(inventory: true)])
        #expect(state.handle(key(.character("l"), .ctrl), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.refresh(inventory: false)])
        _ = state.handle(char("?"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .help)
        _ = state.handle(char("x"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .normal, "any key closes help without acting")
        _ = state.handle(key(.function(1)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .help)
        _ = state.handle(.mouse(MouseEvent(button: .left, action: .press, x: 0, y: 0)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .normal)
        #expect(state.handle(char("q"), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.quit])
        #expect(state.shouldQuit)
    }

    @Test("Ctrl-C quits from every mode")
    func ctrlC() {
        for mode in [InputMode.normal, .filter, .help, .confirm(Confirmation(operation: .prune(.images)))] {
            var state = AppState()
            state.mode = mode
            #expect(state.handle(key(.character("c"), .ctrl), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.quit])
        }
    }

    @Test("Escape clears the filter first, then the notice")
    func escape() {
        var state = AppState()
        state.filterText = "x"
        state.post("hello", level: .info, now: 0)
        _ = state.handle(key(.escape), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.filterText.isEmpty)
        #expect(state.notice != nil)
        _ = state.handle(key(.escape), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.notice == nil)
    }

    @Test("Enter toggles details, or folds a selected compose project")
    func enter() {
        var state = AppState()
        _ = state.handle(key(.enter), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(!state.showDetails)
        state.tab = .compose
        _ = state.handle(key(.enter), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.collapsedProjects == ["shop"])
        _ = state.handle(char("c"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.collapsedProjects.isEmpty)
    }

    @Test("notices expire on schedule")
    func noticeExpiry() {
        var state = AppState()
        state.post("x", level: .warning, now: 10, duration: 2)
        state.expireNotice(now: 11)
        #expect(state.notice != nil)
        state.expireNotice(now: 12)
        #expect(state.notice == nil)
        state.post("sticky", level: .error, now: 10, duration: nil)
        state.expireNotice(now: 1_000_000)
        #expect(state.notice?.text == "sticky")
    }

    @Test("configuration seeds the initial state")
    func fromConfiguration() {
        var configuration = AppConfiguration()
        configuration.sortKey = .name
        configuration.sortDescending = false
        configuration.showStopped = false
        configuration.themeName = "ansi"
        let state = AppState(configuration: configuration)
        #expect(state.sortKey == .name)
        #expect(!state.sortDescending)
        #expect(state.statusFilter == .running)
        #expect(state.themeName == "ansi")
    }

    @Test("reconciliation pins selection to surviving rows")
    func reconcile() {
        var state = AppState()
        state.selectedIDs[.containers] = "id-db-1"
        state.selectedIndexes[.containers] = 2
        var changed = snapshot
        changed.containers.removeAll { $0.name == "db-1" }
        state.reconcileSelection(snapshot: changed)
        #expect(state.selectedIDs[.containers] == "id-old", "falls back to the same index")
        state.reconcileSelection(snapshot: DockerSnapshot())
        #expect(state.selectedIDs[.containers] == nil)
    }
}

@Suite("Reducer: filter mode")
struct ReducerFilterTests {
    let snapshot = AppFixtures.snapshot()

    func type(_ text: String, into state: inout AppState) {
        for character in text {
            _ = state.handle(.key(KeyEvent(.character(character))), snapshot: snapshot, hitMap: HitMap(), now: 0)
        }
    }

    @Test("typing builds the filter and resets selection")
    func typing() {
        var state = AppState()
        state.selectedIDs[.containers] = "id-old"
        type("/web", into: &state)
        #expect(state.mode == .filter)
        #expect(state.filterText == "web")
        #expect(state.selectedIDs[.containers] == nil)
        _ = state.handle(.key(KeyEvent(.backspace)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.filterText == "we")
        _ = state.handle(.key(KeyEvent(.enter)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .normal)
        #expect(state.filterText == "we", "Enter keeps the filter")
    }

    @Test("Escape clears and exits; Ctrl-U and Ctrl-W edit")
    func editing() {
        var state = AppState()
        type("/hello world", into: &state)
        _ = state.handle(.key(KeyEvent(.character("w"), modifiers: .ctrl)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.filterText == "hello ")
        _ = state.handle(.key(KeyEvent(.character("u"), modifiers: .ctrl)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.filterText.isEmpty)
        type("abc", into: &state)
        _ = state.handle(.key(KeyEvent(.escape)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.filterText.isEmpty)
        #expect(state.mode == .normal)
    }

    @Test("filter mode treats command keys as text and ignores Alt chords")
    func literal() {
        var state = AppState()
        type("/qx", into: &state)
        #expect(state.filterText == "qx")
        #expect(!state.shouldQuit)
        _ = state.handle(.key(KeyEvent(.character("z"), modifiers: .alt)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.filterText == "qx")
        _ = state.handle(.key(KeyEvent(.backspace)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(.key(KeyEvent(.backspace)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(.key(KeyEvent(.backspace)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.filterText.isEmpty)
    }

    @Test("paste starts filtering and strips newlines")
    func paste() {
        var state = AppState()
        _ = state.handle(.paste("web\n1\t"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .filter)
        #expect(state.filterText == "web1")
        _ = state.handle(.paste("!"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.filterText == "web1!")
        _ = state.handle(.mouse(MouseEvent(button: .left, action: .press, x: 1, y: 1)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .normal)
    }

    @Test("word deletion helper")
    func deleteWord() {
        #expect(AppState.deletingLastWord("one two") == "one ")
        #expect(AppState.deletingLastWord("one two  ") == "one ")
        #expect(AppState.deletingLastWord("single") == "")
        #expect(AppState.deletingLastWord("") == "")
    }
}

@Suite("Reducer: operations")
struct ReducerOperationTests {
    let snapshot = AppFixtures.snapshot()

    func select(_ id: String, tab: Tab = .containers) -> AppState {
        var state = AppState()
        state.tab = tab
        state.selectedIDs[tab] = id
        return state
    }

    func press(_ character: Character, _ state: inout AppState, hitMap: HitMap = HitMap()) -> [AppCommand] {
        state.handle(.key(KeyEvent(.character(character))), snapshot: snapshot, hitMap: hitMap, now: 0)
    }

    @Test("safe container actions run immediately with a progress notice")
    func safeActions() {
        var running = select("id-cache")
        #expect(press("x", &running) == [.perform(.container(.stop(timeoutSeconds: nil), id: "id-cache", name: "cache"))])
        #expect(running.notice?.text == "Stopping cache…")
        #expect(press("r", &running) == [.perform(.container(.restart(timeoutSeconds: nil), id: "id-cache", name: "cache"))])
        #expect(press("p", &running) == [.perform(.container(.pause, id: "id-cache", name: "cache"))])
        var stopped = select("id-old")
        #expect(press("u", &stopped) == [.perform(.container(.start, id: "id-old", name: "old"))])
    }

    @Test("inapplicable actions explain why instead of doing nothing silently")
    func inapplicable() {
        var running = select("id-cache")
        #expect(press("u", &running).isEmpty)
        #expect(running.notice?.text == "cache is already running")
        #expect(running.notice?.level == .warning)
        var stopped = select("id-old")
        #expect(press("x", &stopped).isEmpty)
        #expect(stopped.notice?.text == "old is not running")
        #expect(press("p", &stopped).isEmpty)
        #expect(press("K", &stopped).isEmpty)
        #expect(press("U", &stopped).isEmpty)
        var empty = AppState()
        #expect(empty.handle(.key(KeyEvent(.character("x"))), snapshot: DockerSnapshot(), hitMap: HitMap(), now: 0).isEmpty)
        #expect(empty.notice?.text == "Nothing selected")
    }

    @Test("destructive actions open a confirmation with No focused")
    func destructive() {
        var state = select("id-cache")
        #expect(press("K", &state).isEmpty)
        guard case .confirm(let confirmation) = state.mode else {
            Issue.record("expected confirmation")
            return
        }
        #expect(confirmation.operation == .container(.kill(signal: nil), id: "id-cache", name: "cache"))
        #expect(!confirmation.confirmFocused)

        var remove = select("id-old")
        _ = press("D", &remove)
        #expect(remove.mode == .confirm(Confirmation(operation: .removeContainer(id: "id-old", name: "old", force: false))))
        var removeRunning = select("id-cache")
        _ = press("D", &removeRunning)
        #expect(removeRunning.mode == .confirm(Confirmation(operation: .removeContainer(id: "id-cache", name: "cache", force: true))))
    }

    @Test("confirmation: y confirms, n/Esc cancel, Enter follows focus")
    func confirmFlow() {
        let operation = DockerOperation.prune(.images)
        var state = AppState()
        state.mode = .confirm(Confirmation(operation: operation))
        #expect(press("y", &state) == [.perform(operation)])
        #expect(state.mode == .normal)
        #expect(state.notice?.text == "Pruning images…")

        for cancel in [InputEvent.key(KeyEvent(.character("n"))), .key(KeyEvent(.escape)), .key(KeyEvent(.character("q")))] {
            state.mode = .confirm(Confirmation(operation: operation))
            #expect(state.handle(cancel, snapshot: snapshot, hitMap: HitMap(), now: 0).isEmpty)
            #expect(state.mode == .normal)
            #expect(!state.shouldQuit, "q cancels the dialog rather than quitting")
        }

        state.mode = .confirm(Confirmation(operation: operation))
        #expect(state.handle(.key(KeyEvent(.enter)), snapshot: snapshot, hitMap: HitMap(), now: 0).isEmpty, "Enter on No cancels")
        state.mode = .confirm(Confirmation(operation: operation))
        _ = state.handle(.key(KeyEvent(.left)), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.mode == .confirm(Confirmation(operation: operation, confirmFocused: true)))
        #expect(state.handle(.key(KeyEvent(.enter)), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.perform(operation)])

        state.mode = .confirm(Confirmation(operation: operation))
        _ = press("j", &state)
        #expect(state.mode == .confirm(Confirmation(operation: operation)), "other keys are ignored")
    }

    @Test("confirmation buttons respond to clicks")
    func confirmClicks() {
        let operation = DockerOperation.removeVolume(name: "v")
        var hitMap = HitMap()
        hitMap.confirmButton = Rect(x: 10, y: 5, width: 6, height: 1)
        hitMap.cancelButton = Rect(x: 20, y: 5, width: 6, height: 1)
        var state = AppState()
        state.mode = .confirm(Confirmation(operation: operation))
        let click = { (x: Int) in InputEvent.mouse(MouseEvent(button: .left, action: .press, x: x, y: 5)) }
        #expect(state.handle(click(0), snapshot: snapshot, hitMap: hitMap, now: 0).isEmpty)
        #expect(state.mode != .normal, "clicks outside the buttons are ignored")
        #expect(state.handle(click(21), snapshot: snapshot, hitMap: hitMap, now: 0).isEmpty)
        #expect(state.mode == .normal)
        state.mode = .confirm(Confirmation(operation: operation))
        #expect(state.handle(click(12), snapshot: snapshot, hitMap: hitMap, now: 0) == [.perform(operation)])
    }

    @Test("compose project rows map keys to stack operations")
    func composeRows() {
        let projectID = "project:shop"
        func run(_ character: Character) -> (commands: [AppCommand], mode: InputMode) {
            var state = select(projectID, tab: .compose)
            let commands = press(character, &state)
            return (commands, state.mode)
        }
        #expect(run("u").commands == [.perform(.compose(.start, project: "shop"))])
        #expect(run("U").commands == [.perform(.compose(.up, project: "shop"))])
        #expect(run("x").commands == [.perform(.compose(.stop, project: "shop"))])
        #expect(run("r").commands == [.perform(.compose(.restart, project: "shop"))])
        #expect(run("p").commands == [.perform(.compose(.pause, project: "shop"))])
        #expect(run("D").mode == .confirm(Confirmation(operation: .compose(.down, project: "shop"))))
        #expect(run("K").commands.isEmpty)
    }

    @Test("resource tabs support removal and prune")
    func resources() {
        var image = select("sha256:aaa", tab: .images)
        _ = press("D", &image)
        #expect(image.mode == .confirm(Confirmation(operation: .removeImage(id: "sha256:aaa", name: "nginx:latest", force: false))))
        var network = select("n2", tab: .networks)
        _ = press("D", &network)
        #expect(network.mode == .confirm(Confirmation(operation: .removeNetwork(id: "n2", name: "shop_default"))))
        var builtIn = select("n1", tab: .networks)
        #expect(press("D", &builtIn).isEmpty)
        #expect(builtIn.notice?.text.contains("built-in") == true)
        var volume = select("volume:shop_data", tab: .volumes)
        _ = press("D", &volume)
        #expect(volume.mode == .confirm(Confirmation(operation: .removeVolume(name: "shop_data"))))
        var other = select("sha256:aaa", tab: .images)
        #expect(press("x", &other).isEmpty)
        #expect(other.notice?.text == "That action does not apply here")

        for (tab, target) in [(Tab.containers, PruneTarget.containers), (.compose, .containers), (.images, .images),
                              (.networks, .networks), (.volumes, .volumes)] {
            var state = AppState()
            state.tab = tab
            _ = press("X", &state)
            #expect(state.mode == .confirm(Confirmation(operation: .prune(target))))
        }
    }

    @Test("unpause via u on a paused container")
    func unpause() {
        var snapshot = self.snapshot
        snapshot.containers.append(AppFixtures.container("frozen", state: .paused))
        var state = AppState()
        state.selectedIDs[.containers] = "id-frozen"
        #expect(state.handle(.key(KeyEvent(.character("u"))), snapshot: snapshot, hitMap: HitMap(), now: 0)
            == [.perform(.container(.unpause, id: "id-frozen", name: "frozen"))])
        #expect(state.handle(.key(KeyEvent(.character("p"))), snapshot: snapshot, hitMap: HitMap(), now: 0)
            == [.perform(.container(.unpause, id: "id-frozen", name: "frozen"))])
    }
}

@Suite("Reducer: mouse")
struct ReducerMouseTests {
    let snapshot = AppFixtures.snapshot()

    func hitMap() -> HitMap {
        var map = HitMap()
        map.tabs = [TabHit(tab: .containers, rect: Rect(x: 0, y: 0, width: 10, height: 1)),
                    TabHit(tab: .images, rect: Rect(x: 12, y: 0, width: 10, height: 1))]
        map.rows = (0..<4).map { RowHit(index: $0, rect: Rect(x: 0, y: 2 + $0, width: 40, height: 1)) }
        map.listRect = Rect(x: 0, y: 1, width: 40, height: 10)
        map.detailRect = Rect(x: 40, y: 1, width: 40, height: 10)
        map.detailPanes = [PaneHit(pane: .logs, rect: Rect(x: 50, y: 2, width: 6, height: 1))]
        return map
    }

    func click(_ x: Int, _ y: Int, button: MouseButton = .left, action: MouseAction = .press) -> InputEvent {
        .mouse(MouseEvent(button: button, action: action, x: x, y: y))
    }

    @Test("clicking tabs, rows and detail panes")
    func clicks() {
        var state = AppState()
        _ = state.handle(click(5, 4), snapshot: snapshot, hitMap: hitMap(), now: 0)
        #expect(ListModel.selectedItem(snapshot: snapshot, state: state)?.container?.name == "db-1")
        _ = state.handle(click(52, 2), snapshot: snapshot, hitMap: hitMap(), now: 0)
        #expect(state.detailPane == .logs)
        _ = state.handle(click(15, 0), snapshot: snapshot, hitMap: hitMap(), now: 0)
        #expect(state.tab == .images)
        _ = state.handle(click(5, 30), snapshot: snapshot, hitMap: hitMap(), now: 0)
        #expect(state.tab == .images, "clicks on nothing do nothing")
        _ = state.handle(click(5, 4, action: .release), snapshot: snapshot, hitMap: hitMap(), now: 0)
        _ = state.handle(click(5, 4, button: .right), snapshot: snapshot, hitMap: hitMap(), now: 0)
    }

    @Test("clicking a selected project row folds it")
    func foldByClick() {
        var state = AppState()
        state.tab = .compose
        _ = state.handle(click(5, 3), snapshot: snapshot, hitMap: hitMap(), now: 0)
        _ = state.handle(click(5, 2), snapshot: snapshot, hitMap: hitMap(), now: 0)
        #expect(state.collapsedProjects.isEmpty, "first click selects")
        _ = state.handle(click(5, 2), snapshot: snapshot, hitMap: hitMap(), now: 0)
        #expect(state.collapsedProjects == ["shop"], "second click folds")
    }

    @Test("the wheel scrolls the list or the detail panel")
    func wheel() {
        var state = AppState()
        _ = state.handle(click(5, 5, button: .wheelDown), snapshot: snapshot, hitMap: hitMap(), now: 0)
        #expect(ListModel.selectedItem(snapshot: snapshot, state: state)?.container?.name == "web-1")
        _ = state.handle(click(5, 5, button: .wheelUp), snapshot: snapshot, hitMap: hitMap(), now: 0)
        #expect(ListModel.selectedItem(snapshot: snapshot, state: state)?.container?.name == "cache")
        _ = state.handle(click(45, 5, button: .wheelDown), snapshot: snapshot, hitMap: hitMap(), now: 0)
        #expect(state.detailScroll == 3)
        _ = state.handle(click(45, 5, button: .wheelUp), snapshot: snapshot, hitMap: hitMap(), now: 0)
        _ = state.handle(click(45, 5, button: .wheelUp), snapshot: snapshot, hitMap: hitMap(), now: 0)
        #expect(state.detailScroll == 0, "never negative")
    }

    @Test("focus events are ignored")
    func focus() {
        var state = AppState()
        #expect(state.handle(.focus(true), snapshot: snapshot, hitMap: hitMap(), now: 0).isEmpty)
        #expect(state == AppState())
    }
}

@Suite("Docker operations")
struct DockerOperationTests {
    @Test("destructive classification", arguments: [
        (DockerOperation.container(.kill(signal: nil), id: "a", name: "a"), true),
        (.container(.stop(timeoutSeconds: nil), id: "a", name: "a"), false),
        (.container(.start, id: "a", name: "a"), false),
        (.removeContainer(id: "a", name: "a", force: false), true),
        (.compose(.down, project: "p"), true),
        (.compose(.up, project: "p"), false),
        (.removeImage(id: "i", name: "i", force: false), true),
        (.removeNetwork(id: "n", name: "n"), true),
        (.removeVolume(name: "v"), true),
        (.prune(.volumes), true)
    ])
    func destructive(operation: DockerOperation, expected: Bool) {
        #expect(operation.isDestructive == expected)
    }

    @Test("every operation has non-empty user-facing text")
    func text() {
        let operations: [DockerOperation] = [
            .container(.start, id: "a", name: "web"), .container(.stop(timeoutSeconds: nil), id: "a", name: "web"),
            .container(.restart(timeoutSeconds: nil), id: "a", name: "web"), .container(.kill(signal: nil), id: "a", name: "web"),
            .container(.pause, id: "a", name: "web"), .container(.unpause, id: "a", name: "web"),
            .removeContainer(id: "a", name: "web", force: true), .removeContainer(id: "a", name: "web", force: false),
            .compose(.down, project: "p"), .compose(.restart, project: "p"),
            .removeImage(id: "i", name: "img", force: true), .removeImage(id: "i", name: "img", force: false),
            .removeNetwork(id: "n", name: "net"), .removeVolume(name: "vol")
        ] + PruneTarget.allCases.map { DockerOperation.prune($0) }
        for operation in operations {
            #expect(!operation.confirmationTitle.isEmpty)
            #expect(!operation.confirmationMessage.isEmpty)
            #expect(operation.progressDescription.hasSuffix("…"))
            #expect(!operation.successDescription.isEmpty)
        }
        #expect(DockerOperation.container(.kill(signal: nil), id: "a", name: "web").confirmationMessage.contains("SIGKILL"))
        #expect(DockerOperation.removeVolume(name: "v").confirmationMessage.contains("permanently"))
        #expect(DockerOperation.container(.unpause, id: "a", name: "web").successDescription == "Resumed web")
        #expect(!DockerOperation.container(.start, id: "a", name: "w").touchesInventory)
        #expect(DockerOperation.prune(.images).touchesInventory)
        #expect("hello".capitalizedFirst == "Hello")
        #expect("".capitalizedFirst == "")
    }

    @Test("enums cycle through all cases")
    func cycling() {
        #expect(Tab.volumes.next() == .containers)
        #expect(Tab.containers.previous() == .volumes)
        #expect(Tab.allCases.map(\.shortcut) == ["1", "2", "3", "4", "5"])
        #expect(Tab.images.usesInventory && !Tab.compose.usesInventory)
        #expect(SortKey.image.next() == .name)
        #expect(StatusFilter.stopped.next() == .all)
        #expect(DetailPane.logs.next() == .overview)
        #expect(Set(Tab.allCases.map(\.title)).count == Tab.allCases.count)
        #expect(Set(DetailPane.allCases.map(\.title)).count == DetailPane.allCases.count)
    }
}
