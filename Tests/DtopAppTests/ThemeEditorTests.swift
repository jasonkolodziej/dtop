import Foundation
import Testing
@testable import DtopApp
@testable import DtopCore
@testable import DtopUI

private func key(_ key: Key, _ modifiers: KeyModifiers = []) -> InputEvent { .key(KeyEvent(key, modifiers: modifiers)) }
private func char(_ character: Character) -> InputEvent { key(.character(character)) }

private func type(_ text: String, into editor: inout ThemeEditor) {
    for character in text { _ = editor.handle(char(character)) }
}

private func select(_ row: ThemeEditor.Row, in editor: inout ThemeEditor) {
    editor.selected = ThemeEditor.rows.firstIndex(of: row)!
}

@Suite("Theme editor: editing")
struct ThemeEditorEditingTests {
    func editor(_ current: Theme = .harbor) -> ThemeEditor {
        ThemeEditor(current: current, available: Theme.builtIn)
    }

    @Test("opens on the current theme with nothing modified")
    func opens() {
        let editor = editor()
        #expect(editor.draft == .harbor)
        #expect(editor.base == .harbor)
        #expect(!editor.isDirty)
        #expect(editor.selectedRow == .base)
        #expect(ThemeEditor.rows.count == 3 + ThemeRole.allCases.count)
    }

    @Test("a custom current theme is offered alongside the built-ins")
    func customCurrent() {
        var custom = Theme.midnight
        custom.name = "mine"
        let editor = ThemeEditor(current: custom, available: Theme.builtIn)
        #expect(editor.available.first?.name == "mine")
        #expect(editor.base.name == "mine")
        #expect(editor.defaultSaveName == "mine")
    }

    @Test("navigation clamps to the row list")
    func navigation() {
        var editor = editor()
        _ = editor.handle(key(.up))
        #expect(editor.selected == 0)
        _ = editor.handle(char("j"))
        _ = editor.handle(key(.down))
        #expect(editor.selected == 2)
        _ = editor.handle(key(.pageDown))
        #expect(editor.selected == 12)
        _ = editor.handle(char("G"))
        #expect(editor.selected == ThemeEditor.rows.count - 1)
        _ = editor.handle(key(.pageDown))
        #expect(editor.selected == ThemeEditor.rows.count - 1)
        _ = editor.handle(key(.home))
        #expect(editor.selected == 0)
        _ = editor.handle(.mouse(MouseEvent(button: .wheelDown, action: .press, x: 0, y: 0)))
        #expect(editor.selected == 1)
        _ = editor.handle(.mouse(MouseEvent(button: .wheelUp, action: .press, x: 0, y: 0)))
        #expect(editor.selected == 0)
    }

    @Test("the theme row switches the whole draft between available themes")
    func baseCycling() {
        var editor = editor()
        _ = editor.handle(key(.right))
        #expect(editor.draft == Theme.builtIn[1])
        #expect(editor.base == Theme.builtIn[1])
        #expect(editor.isDirty)
        _ = editor.handle(key(.left))
        _ = editor.handle(key(.left))
        #expect(editor.draft == Theme.builtIn.last!, "wraps around")
        _ = editor.handle(key(.enter))
        #expect(editor.draft == .harbor, "Enter also advances")
        #expect(!editor.isDirty)
    }

    @Test("border and emphasis rows cycle their options")
    func borderAndEmphasis() {
        var editor = editor()
        select(.border, in: &editor)
        _ = editor.handle(key(.right))
        #expect(editor.draft.border == .single)
        _ = editor.handle(key(.left))
        _ = editor.handle(key(.left))
        #expect(editor.draft.border == .ascii)
        #expect(editor.valueText(for: .border) == "‹ ascii ›")
        select(.emphasis, in: &editor)
        _ = editor.handle(key(.enter))
        #expect(editor.draft.usesAttributesForEmphasis)
        #expect(editor.valueText(for: .emphasis) == "‹ attributes ›")
        #expect(editor.isModified(.emphasis))
        #expect(editor.isModified(.border))
        #expect(!editor.isModified(.base))
    }

    @Test("arrows adjust the selected RGB channel; shift takes big steps; values clamp")
    func channels() {
        var editor = editor()
        select(.role(.accent), in: &editor) // harbor accent = #4fb0f5
        _ = editor.handle(key(.right))
        #expect(editor.draft[.accent] == .rgb(0x4F + 8, 0xB0, 0xF5))
        _ = editor.handle(key(.tab))
        #expect(editor.channel == .green)
        _ = editor.handle(key(.left, .shift))
        #expect(editor.draft[.accent] == .rgb(0x57, 0xB0 - 32, 0xF5))
        _ = editor.handle(key(.backTab, .shift))
        _ = editor.handle(key(.backTab, .shift))
        #expect(editor.channel == .blue)
        for _ in 0..<5 { _ = editor.handle(char("L")) }
        #expect(editor.draft[.accent] == .rgb(0x57, 0x90, 255), "clamped at 255")
        #expect(editor.isModified(.role(.accent)))
        #expect(!editor.isModified(.role(.memory)))
    }

    @Test("adjusting a default or palette color converts it to RGB")
    func defaultToRGB() {
        var editor = editor()
        select(.role(.background), in: &editor) // harbor background = default
        _ = editor.handle(key(.right))
        guard case .rgb(let r, _, _) = editor.draft[.background] else {
            Issue.record("expected rgb, got \(editor.draft[.background])")
            return
        }
        #expect(r == 0xD8 + 8, "starts from the foreground color")
        var ansi = ThemeEditor(current: .ansi, available: Theme.builtIn)
        select(.role(.statusRunning), in: &ansi) // ansi green
        _ = ansi.handle(key(.right))
        #expect(ansi.draft[.statusRunning] == .rgb(8, 205, 0))
    }

    @Test("typing a color accepts every notation and rejects junk")
    func colorInput() {
        var editor = editor()
        select(.role(.error), in: &editor)
        _ = editor.handle(key(.enter))
        #expect(editor.field == .colorInput("#e55f5f"))
        _ = editor.handle(key(.character("u"), .ctrl))
        type("#123456", into: &editor)
        _ = editor.handle(key(.enter))
        #expect(editor.draft[.error] == .rgb(0x12, 0x34, 0x56))
        #expect(editor.field == .none)

        for (text, color) in [("red", Color.ansi(1)), ("color(42)", .indexed(42)), ("default", .default), ("#fff", .rgb(255, 255, 255))] {
            _ = editor.handle(key(.enter))
            _ = editor.handle(key(.character("u"), .ctrl))
            type(text, into: &editor)
            _ = editor.handle(key(.enter))
            #expect(editor.draft[.error] == color, "\(text)")
        }

        _ = editor.handle(key(.enter))
        _ = editor.handle(key(.character("u"), .ctrl))
        type("blurple", into: &editor)
        _ = editor.handle(key(.enter))
        #expect(editor.field == .colorInput("blurple"), "stays open on invalid input")
        #expect(editor.message?.contains("not a color") == true)
        _ = editor.handle(key(.backspace))
        #expect(editor.field == .colorInput("blurpl"))
        _ = editor.handle(key(.escape))
        #expect(editor.field == .none)
        #expect(editor.draft[.error] == .rgb(255, 255, 255), "escape keeps the last applied value")
    }

    @Test("pasting a color opens the input with the pasted text")
    func pasteColor() {
        var editor = editor()
        select(.role(.accent), in: &editor)
        _ = editor.handle(.paste("  #abcdef\n"))
        #expect(editor.field == .colorInput("#abcdef"))
        _ = editor.handle(.paste("00"))
        #expect(editor.field == .colorInput("#abcdef00"))
        select(.border, in: &editor)
        editor.field = .none
        _ = editor.handle(.paste("#000000"))
        #expect(editor.field == .none, "pasting on a non-color row is ignored")
    }

    @Test("d resets the selected row to the base theme; r reverts everything")
    func reset() {
        var editor = editor()
        select(.role(.accent), in: &editor)
        _ = editor.handle(key(.right))
        select(.role(.memory), in: &editor)
        _ = editor.handle(key(.right))
        _ = editor.handle(char("d"))
        #expect(editor.draft[.memory] == Theme.harbor[.memory])
        #expect(editor.draft[.accent] != Theme.harbor[.accent])
        select(.border, in: &editor)
        _ = editor.handle(key(.right))
        _ = editor.handle(key(.delete))
        #expect(editor.draft.border == Theme.harbor.border)
        select(.base, in: &editor)
        _ = editor.handle(key(.right))
        _ = editor.handle(char("r"))
        #expect(editor.draft == .harbor)
        #expect(editor.base == .harbor)
        #expect(!editor.isDirty)
        #expect(editor.message == "All changes reverted")
    }

    @Test("clicking a row selects it; clicking the selected row changes it")
    func clicks() {
        var editor = editor()
        let hits = (0..<5).map { RowHit(index: $0, rect: Rect(x: 0, y: 10 + $0, width: 40, height: 1)) }
        let click = { (y: Int) in InputEvent.mouse(MouseEvent(button: .left, action: .press, x: 5, y: y)) }
        _ = editor.handle(click(11), rowHits: hits)
        #expect(editor.selectedRow == .border)
        _ = editor.handle(click(11), rowHits: hits)
        #expect(editor.draft.border == .single)
        _ = editor.handle(click(30), rowHits: hits)
        #expect(editor.selectedRow == .border)
    }
}

@Suite("Theme editor: save and discard")
struct ThemeEditorSaveTests {
    @Test("closing without changes cancels immediately")
    func cleanClose() {
        var editor = ThemeEditor(current: .harbor, available: Theme.builtIn)
        #expect(editor.handle(key(.escape)) == .cancelled)
        var other = ThemeEditor(current: .harbor, available: Theme.builtIn)
        #expect(other.handle(char("q")) == .cancelled)
    }

    @Test("closing with changes asks first; n keeps editing, y discards, s goes to save")
    func confirmDiscard() {
        var editor = ThemeEditor(current: .harbor, available: Theme.builtIn)
        _ = editor.handle(key(.right))
        #expect(editor.handle(key(.escape)) == .editing)
        #expect(editor.field == .confirmDiscard)
        #expect(editor.handle(char("x")) == .editing)
        #expect(editor.handle(char("n")) == .editing)
        #expect(editor.field == .none)
        #expect(editor.isDirty, "still holding the draft")
        _ = editor.handle(key(.escape))
        _ = editor.handle(char("s"))
        #expect(editor.field == .saveName(editor.defaultSaveName))
        _ = editor.handle(key(.escape))
        _ = editor.handle(key(.escape))
        #expect(editor.handle(char("y")) == .cancelled)
    }

    @Test("default save names avoid overwriting built-ins")
    func defaultNames() {
        var editor = ThemeEditor(current: .harbor, available: Theme.builtIn)
        #expect(editor.defaultSaveName == "harbor", "an unmodified built-in can simply be selected")
        select(.role(.accent), in: &editor)
        _ = editor.handle(key(.right))
        #expect(editor.defaultSaveName == "harbor-custom")
    }

    @Test("saving returns the draft under the typed name")
    func save() {
        var editor = ThemeEditor(current: .harbor, available: Theme.builtIn)
        select(.role(.accent), in: &editor)
        _ = editor.handle(key(.right))
        _ = editor.handle(char("s"))
        for _ in 0..<20 { _ = editor.handle(key(.backspace)) }
        type("My_Theme-2", into: &editor)
        #expect(editor.field == .saveName("my_theme-2"), "lower-cased")
        type("!@ ", into: &editor)
        #expect(editor.field == .saveName("my_theme-2"), "invalid characters are ignored")
        guard case .save(let name, let theme) = editor.handle(key(.enter)) else {
            Issue.record("expected save")
            return
        }
        #expect(name == "my_theme-2")
        #expect(theme.name == "my_theme-2")
        #expect(theme[.accent] == editor.draft[.accent])
    }

    @Test("name validation", arguments: [
        ("", "Enter a name"), ("default", "reserved"), ("midnight", "built-in")
    ])
    func validation(name: String, expected: String) {
        var draft = Theme.harbor
        draft.colors[.accent] = .rgb(1, 2, 3)
        #expect(ThemeEditor.validate(name: name, draft: draft)?.contains(expected) == true)
    }

    @Test("an unmodified built-in may be saved under its own name (just selects it)")
    func selectBuiltIn() {
        #expect(ThemeEditor.validate(name: "midnight", draft: .midnight) == nil)
        #expect(ThemeEditor.validate(name: "has space", draft: .harbor) != nil)
        #expect(ThemeEditor.validate(name: "ok-name_1", draft: .harbor) == nil)
        var editor = ThemeEditor(current: .harbor, available: Theme.builtIn)
        _ = editor.handle(key(.right))
        _ = editor.handle(char("s"))
        #expect(editor.field == .saveName(Theme.builtIn[1].name))
        #expect(editor.handle(key(.enter)) == .save(name: Theme.builtIn[1].name, theme: Theme.builtIn[1]))
    }

    @Test("invalid names keep the prompt open with a reason")
    func invalidName() {
        var editor = ThemeEditor(current: .harbor, available: Theme.builtIn)
        select(.role(.accent), in: &editor)
        _ = editor.handle(key(.right))
        _ = editor.handle(char("s"))
        for _ in 0..<20 { _ = editor.handle(key(.backspace)) }
        type("harbor", into: &editor)
        #expect(editor.handle(key(.enter)) == .editing)
        #expect(editor.message?.contains("built-in") == true)
        _ = editor.handle(.paste(" Mine 2!"))
        #expect(editor.field == .saveName("harbormine2"))
    }
}

@Suite("Theme store")
struct ThemeStoreTests {
    final class MemoryFS: @unchecked Sendable {
        let lock = NSLock()
        var files: [String: String] = [:]
        var directories: Set<String> = []
        var failWrites = false

        var fileSystem: ThemeFileSystem {
            ThemeFileSystem(
                read: { path in self.lock.lock(); defer { self.lock.unlock() }; return self.files[path] },
                write: { path, contents in
                    self.lock.lock(); defer { self.lock.unlock() }
                    if self.failWrites { throw CocoaError(.fileWriteNoPermission) }
                    self.files[path] = contents
                },
                createDirectory: { path in self.lock.lock(); self.directories.insert(path); self.lock.unlock() }
            )
        }
    }

    @Test("saving a custom theme writes the theme file and points dtop.conf at it")
    func saveCustom() throws {
        let fs = MemoryFS()
        fs.files["/cfg/dtop.conf"] = "# my settings\ninterval = 1s\ntheme = harbor\nmouse = true\n"
        var theme = Theme.harbor
        theme.name = "ocean"
        theme.colors[.accent] = .rgb(1, 2, 3)
        let store = ThemeStore(configDirectory: "/cfg", platform: .linux)
        let result = try store.save(theme, name: "ocean", fileSystem: fs.fileSystem)
        #expect(result == ThemeSaveResult(themeFile: "/cfg/themes/ocean.theme", configFile: "/cfg/dtop.conf"))
        #expect(fs.directories.contains("/cfg/themes"))
        #expect(fs.files["/cfg/dtop.conf"] == "# my settings\ninterval = 1s\ntheme = ocean\nmouse = true\n")
        let parsed = try ThemeFile.parse(try #require(fs.files["/cfg/themes/ocean.theme"]), fallbackName: "x")
        #expect(parsed == theme, "the saved file round-trips through the theme loader")
    }

    @Test("selecting an unmodified built-in only updates the config")
    func saveBuiltIn() throws {
        let fs = MemoryFS()
        let result = try ThemeStore(configDirectory: "/cfg", platform: .linux)
            .save(.forest, name: "forest", fileSystem: fs.fileSystem)
        #expect(result.themeFile == nil)
        #expect(fs.files.keys.sorted() == ["/cfg/dtop.conf"])
        #expect(fs.files["/cfg/dtop.conf"] == "theme = forest\n")
        #expect(fs.directories.contains("/cfg"))
    }

    @Test("an explicit --config file is the one updated")
    func explicitConfig() throws {
        let fs = MemoryFS()
        fs.files["/elsewhere/my.conf"] = "theme=daylight"
        let store = ThemeStore(configDirectory: "/cfg", configFile: "/elsewhere/my.conf", platform: .linux)
        _ = try store.save(.ember, name: "ember", fileSystem: fs.fileSystem)
        #expect(fs.files["/elsewhere/my.conf"] == "theme = ember\n")
    }

    @Test("failures produce actionable errors")
    func failures() {
        let fs = MemoryFS()
        fs.failWrites = true
        #expect(throws: ConfigurationError.self) {
            _ = try ThemeStore(configDirectory: "/cfg", platform: .linux).save(.harbor, name: "x", fileSystem: fs.fileSystem)
        }
        do {
            _ = try ThemeStore(configDirectory: nil, platform: .linux).save(.harbor, name: "x", fileSystem: fs.fileSystem)
            Issue.record("expected an error")
        } catch {
            #expect(String(describing: error).contains("XDG_CONFIG_HOME"))
        }
    }

    @Test("Windows paths use backslashes")
    func windowsPaths() throws {
        let fs = MemoryFS()
        var theme = Theme.harbor
        theme.colors[.accent] = .red
        let result = try ThemeStore(configDirectory: #"C:\Users\me\AppData\Roaming\dtop"#, platform: .windows)
            .save(theme, name: "red", fileSystem: fs.fileSystem)
        #expect(result.themeFile == #"C:\Users\me\AppData\Roaming\dtop\themes\red.theme"#)
        #expect(result.configFile == #"C:\Users\me\AppData\Roaming\dtop\dtop.conf"#)
    }

    @Test("config editing replaces, de-duplicates, appends and preserves comments", arguments: [
        ("", "theme = x\n"),
        ("interval = 2", "interval = 2\ntheme = x\n"),
        ("theme = a\n", "theme = x\n"),
        ("Theme=a\n# theme = commented\ntheme = b\nmouse = on", "theme = x\n# theme = commented\nmouse = on\n"),
        ("; note\n\nsort = cpu\n", "; note\n\nsort = cpu\ntheme = x\n"),
        ("themes = keep\n", "themes = keep\ntheme = x\n")
    ])
    func settingValue(input: String, expected: String) {
        #expect(ThemeStore.settingValue(in: input, key: "theme", to: "x") == expected)
    }

    @Test("an edited config still parses")
    func editedConfigParses() throws {
        let text = ThemeStore.settingValue(in: "interval = 1s\nmouse = off\n", key: "theme", to: "dusk")
        var configuration = AppConfiguration()
        try configuration.apply(configFile: text)
        #expect(configuration.themeName == "dusk")
        #expect(!configuration.mouse)
    }
}

@Suite("Theme editor: app integration")
struct ThemeEditorIntegrationTests {
    let snapshot = AppFixtures.snapshot()

    @Test("T and F2 ask the runtime to open the menu")
    func open() {
        var state = AppState()
        #expect(state.handle(char("T"), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.openThemeEditor])
        #expect(state.handle(key(.function(2)), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.openThemeEditor])
        state.openThemeEditor(current: .harbor, available: Theme.builtIn)
        #expect(state.themeEditor != nil)
    }

    @Test("keys go to the editor, not the list, while it is open")
    func routing() {
        var state = AppState()
        state.openThemeEditor(current: .harbor, available: Theme.builtIn)
        #expect(state.handle(char("q"), snapshot: snapshot, hitMap: HitMap(), now: 0).isEmpty)
        #expect(state.mode == .normal, "q closes the unmodified editor instead of quitting")
        #expect(!state.shouldQuit)

        state.openThemeEditor(current: .harbor, available: Theme.builtIn)
        _ = state.handle(char("j"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(state.themeEditor?.selected == 1)
        #expect(state.selectedIDs.isEmpty, "the container list did not move")
        #expect(state.handle(key(.character("c"), .ctrl), snapshot: snapshot, hitMap: HitMap(), now: 0) == [.quit])
    }

    @Test("discarding returns to normal mode and posts a notice")
    func discard() {
        var state = AppState()
        state.openThemeEditor(current: .harbor, available: Theme.builtIn)
        _ = state.handle(key(.right), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(key(.escape), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(char("y"), snapshot: snapshot, hitMap: HitMap(), now: 5)
        #expect(state.mode == .normal)
        #expect(state.themeName == "harbor", "the original theme is kept")
        #expect(state.notice?.text == "Theme changes discarded")
    }

    @Test("saving emits a command; success closes the menu, failure keeps the draft open")
    func saveFlow() {
        var state = AppState()
        state.openThemeEditor(current: .harbor, available: Theme.builtIn)
        _ = state.handle(key(.right), snapshot: snapshot, hitMap: HitMap(), now: 0)
        _ = state.handle(char("s"), snapshot: snapshot, hitMap: HitMap(), now: 0)
        let commands = state.handle(key(.enter), snapshot: snapshot, hitMap: HitMap(), now: 0)
        #expect(commands == [.saveTheme(name: Theme.builtIn[1].name, theme: Theme.builtIn[1])])
        #expect(state.themeEditor != nil, "stays open until the save result arrives")

        var failed = state
        failed.finishThemeSave(name: "x", error: "disk full", location: nil, now: 0)
        #expect(failed.themeEditor?.message == "disk full")
        #expect(failed.themeEditor?.field == ThemeEditor.Field.none)
        #expect(failed.themeEditor?.isDirty == true)

        state.finishThemeSave(name: "daylight", error: nil, location: "/cfg/dtop.conf", now: 0)
        #expect(state.mode == .normal)
        #expect(state.themeName == "daylight")
        #expect(state.notice?.text == "Saved theme 'daylight' to /cfg/dtop.conf")
        #expect(state.notice?.level == .success)

        var normal = AppState()
        normal.finishThemeSave(name: "x", error: nil, location: nil, now: 0)
        #expect(normal.mode == .normal, "a stray result outside the editor is ignored")
    }
}

@Suite("Theme editor: rendering")
struct ThemeEditorViewTests {
    func render(_ state: AppState, columns: Int = 140, rows: Int = 40) -> RenderedFrame {
        AppView.render(RenderContext(state: state, snapshot: AppFixtures.snapshot(), theme: .harbor,
                                     size: TerminalSize(columns: columns, rows: rows), now: 1_000))
    }

    @Test("the panel lists rows, values and hints, and reports row hits")
    func panel() {
        var state = AppState()
        state.openThemeEditor(current: .harbor, available: Theme.builtIn)
        let frame = render(state)
        let text = frame.buffer.plainText
        #expect(text.contains("theme · harbor"))
        #expect(text.contains("‹ harbor ›"))
        #expect(text.contains("‹ rounded ›"))
        #expect(text.contains("accent"))
        #expect(text.contains("#4fb0f5"))
        #expect(text.contains("● running"))
        #expect(text.contains("esc close"))
        #expect(text.contains("theme menu · changes preview live"))
        #expect(!frame.hitMap.editorRows.isEmpty)
        #expect(frame.hitMap.editorRows.first?.index == 0)
    }

    @Test("the entire UI renders with the draft theme while editing")
    func livePreview() throws {
        var state = AppState()
        state.openThemeEditor(current: .harbor, available: Theme.builtIn)
        guard case .themeEditor(var editor) = state.mode else { return }
        editor.selected = ThemeEditor.rows.firstIndex(of: .role(.headerBackground))!
        editor.field = .colorInput("")
        for character in "#ff0000" { _ = editor.handle(.key(KeyEvent(.character(character)))) }
        _ = editor.handle(.key(KeyEvent(.enter)))
        state.mode = .themeEditor(editor)
        let frame = render(state)
        #expect(frame.buffer[0, 0].style.background == .rgb(255, 0, 0), "the header behind the panel changed")
        #expect(frame.buffer.plainText.contains("unsaved"))

        state.mode = .normal
        #expect(render(state).buffer[0, 0].style.background == Theme.harbor[.headerBackground], "closing reverts")
    }

    @Test("the selected color row shows its RGB channels with the active one highlighted")
    func channels() {
        var state = AppState()
        state.openThemeEditor(current: .harbor, available: Theme.builtIn)
        guard case .themeEditor(var editor) = state.mode else { return }
        editor.selected = ThemeEditor.rows.firstIndex(of: .role(.accent))!
        editor.channel = .green
        state.mode = .themeEditor(editor)
        let frame = render(state)
        let row = frame.buffer.lines.first { $0.contains("#4fb0f5") && $0.contains("R 79") } ?? ""
        #expect(row.contains("G176"))
        #expect(row.contains("B245"))
    }

    @Test("prompts render for color input, save name and discard confirmation", arguments: [
        (ThemeEditor.Field.colorInput("#12"), "color: #12"),
        (.saveName("ocean"), "save as: ocean"),
        (.confirmDiscard, "Discard unsaved theme changes?")
    ])
    func prompts(field: ThemeEditor.Field, expected: String) {
        var state = AppState()
        state.openThemeEditor(current: .harbor, available: Theme.builtIn)
        guard case .themeEditor(var editor) = state.mode else { return }
        editor.field = field
        state.mode = .themeEditor(editor)
        #expect(render(state).buffer.plainText.contains(expected))
    }

    @Test("the panel scrolls to keep deep rows visible and survives small terminals")
    func scrollingAndSizes() {
        var state = AppState()
        state.openThemeEditor(current: .harbor, available: Theme.builtIn)
        guard case .themeEditor(var editor) = state.mode else { return }
        editor.selected = ThemeEditor.rows.count - 1
        state.mode = .themeEditor(editor)
        let frame = render(state, columns: 120, rows: 30)
        #expect(frame.buffer.plainText.contains("dialog border"))
        #expect(frame.hitMap.editorRows.contains { $0.index == ThemeEditor.rows.count - 1 })
        for (columns, rows) in [(40, 10), (60, 14), (80, 24), (200, 60)] {
            for theme in Theme.builtIn {
                var themed = AppState()
                themed.openThemeEditor(current: theme, available: Theme.builtIn)
                let small = render(themed, columns: columns, rows: rows)
                #expect(small.buffer.width == columns)
            }
        }
    }
}
