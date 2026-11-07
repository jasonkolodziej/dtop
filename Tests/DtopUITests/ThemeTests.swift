import Testing
@testable import DtopUI

@Suite("Themes")
struct ThemeTests {
    @Test("every built-in theme defines every role", arguments: Theme.builtIn.map(\.name))
    func complete(name: String) throws {
        let theme = try #require(Theme.named(name))
        for role in ThemeRole.allCases {
            #expect(theme.colors[role] != nil, "\(name) is missing \(role.rawValue)")
        }
    }

    @Test("built-in names are unique and 'default' maps to harbor")
    func names() {
        let names = Theme.builtIn.map(\.name)
        #expect(Set(names).count == names.count)
        #expect(Theme.named("default") == .harbor)
        #expect(Theme.named(" MIDNIGHT ") == .midnight)
        #expect(Theme.named("nope") == nil)
    }

    @Test("the ansi theme stays within the 16-color palette")
    func ansiPalette() {
        for color in Theme.ansi.colors.values {
            switch color {
            case .ansi, .default: continue
            default: Issue.record("ansi theme uses \(color)")
            }
        }
    }

    @Test("monochrome relies on attributes for emphasis")
    func monochrome() {
        #expect(Theme.monochrome.usesAttributesForEmphasis)
        #expect(Theme.monochrome.selection.attributes.contains(.reverse))
        #expect(!Theme.harbor.selection.attributes.contains(.reverse))
        #expect(Theme.monochrome.colors.values.allSatisfy { $0 == .default })
    }

    @Test("gradient runs low → mid → high")
    func gradient() {
        let theme = Theme.harbor
        #expect(theme.gradient(0) == theme[.graphLow])
        #expect(theme.gradient(0.5) == theme[.graphMid])
        #expect(theme.gradient(1) == theme[.graphHigh])
        #expect(theme.gradient(-4) == theme[.graphLow])
        #expect(theme.gradient(.nan) == theme[.graphLow])
        #expect(theme.gradient(0.25) != theme[.graphLow])
    }

    @Test("styles combine role and background")
    func styles() {
        let theme = Theme.midnight
        #expect(theme.style(.accent) == Style(foreground: theme[.accent], background: theme[.background]))
        #expect(theme.base.foreground == theme[.foreground])
        var sparse = theme
        sparse.colors = [:]
        #expect(sparse[.accent] == .default)
    }
}

@Suite("Theme files")
struct ThemeFileTests {
    @Test("parses keys, comments, quotes and inheritance")
    func parse() throws {
        let text = """
        # my theme
        ; also a comment
        // and this
        name = "Ocean"
        inherits = midnight
        accent = #112233
        status-running = green
        border_style = double
        emphasis = attributes

        """
        let theme = try ThemeFile.parse(text, fallbackName: "file")
        #expect(theme.name == "ocean")
        #expect(theme[.accent] == .rgb(0x11, 0x22, 0x33))
        #expect(theme[.statusRunning] == .ansi(2))
        #expect(theme[.memory] == Theme.midnight[.memory])
        #expect(theme.border == .double)
        #expect(theme.usesAttributesForEmphasis)
    }

    @Test("assignments before 'inherits' still win")
    func orderIndependent() throws {
        let theme = try ThemeFile.parse("accent = red\ninherits = daylight", fallbackName: "x")
        #expect(theme[.accent] == .ansi(1))
        #expect(theme[.foreground] == Theme.daylight[.foreground])
        #expect(theme.name == "x")
    }

    @Test("reports the offending line", arguments: [
        ("accent #fff", 1), ("\nwhatever = red", 2), ("accent = notacolor", 1), ("inherits = nowhere", 1),
        ("border_style = wiggly", 1), ("emphasis = loud", 1), ("name =", 1)
    ])
    func errors(text: String, line: Int) {
        #expect {
            _ = try ThemeFile.parse(text, fallbackName: "x")
        } throws: { error in
            (error as? ThemeParseError)?.line == line
        }
    }

    @Test("error descriptions include line numbers")
    func errorDescription() {
        #expect(ThemeParseError(line: 3, message: "bad").description == "theme line 3: bad")
    }

    @Test("serialize then parse reproduces each built-in theme", arguments: Theme.builtIn.map(\.name))
    func roundTrip(name: String) throws {
        let theme = try #require(Theme.named(name))
        let text = ThemeFile.serialize(theme)
        #expect(text.hasPrefix("# dtop theme\n"))
        let parsed = try ThemeFile.parse(text, fallbackName: "ignored")
        #expect(parsed == theme)
    }
}

@Suite("Terminal helpers")
struct TerminalHelperTests {
    @Test("size falls back to COLUMNS/LINES and then 80x24")
    func size() {
        #expect(TerminalSize.fromEnvironment(["COLUMNS": "132", "LINES": "43"]) == TerminalSize(columns: 132, rows: 43))
        #expect(TerminalSize.fromEnvironment(["COLUMNS": "132"]) == nil)
        #expect(TerminalSize.fromEnvironment(["COLUMNS": "0", "LINES": "10"]) == nil)
        #expect(TerminalSize.fallback == TerminalSize(columns: 80, rows: 24))
    }

    @Test("terminal errors are actionable")
    func errors() {
        #expect(TerminalError.notATerminal.description.contains("--once"))
        #expect(TerminalError.configurationFailed("x").description.contains("x"))
    }

    @Test("a terminal object can be created and queried without a TTY")
    func headless() {
        let terminal = Terminal()
        let size = terminal.size(environment: ["COLUMNS": "100", "LINES": "30"])
        #expect(size.columns > 0 && size.rows > 0)
        terminal.restore()
        terminal.write("")
    }
}
