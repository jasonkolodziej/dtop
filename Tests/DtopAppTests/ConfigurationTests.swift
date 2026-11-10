import Testing
@testable import DtopApp
@testable import DtopCore
@testable import DtopUI

@Suite("Command line")
struct CommandLineTests {
    func parse(_ arguments: [String]) throws -> AppConfiguration {
        var configuration = AppConfiguration()
        try configuration.apply(arguments: arguments)
        return configuration
    }

    @Test("defaults are sensible")
    func defaults() {
        let configuration = AppConfiguration()
        #expect(configuration.mode == .interactive)
        #expect(configuration.refreshInterval == 2)
        #expect(configuration.themeName == "harbor")
        #expect(configuration.colorMode == nil)
        #expect(configuration.mouse)
        #expect(configuration.showStopped)
        #expect(configuration.sortKey == .cpu)
        #expect(configuration.host == nil)
    }

    @Test("parses every option in short, long and = forms")
    func options() throws {
        let configuration = try parse([
            "-H", "tcp://h:1", "--interval=500ms", "-t", "midnight", "--color", "256", "--running",
            "--sort", "memory", "--no-mouse", "--history", "120", "-c", "/tmp/x.conf"
        ])
        #expect(configuration.host == "tcp://h:1")
        #expect(configuration.refreshInterval == 0.5)
        #expect(configuration.themeName == "midnight")
        #expect(configuration.colorMode == .palette256)
        #expect(!configuration.showStopped)
        #expect(configuration.sortKey == .memory)
        #expect(!configuration.mouse)
        #expect(configuration.historySize == 120)
        #expect(configuration.configFilePath == "/tmp/x.conf")
        #expect(try parse(["--host=unix:///x", "--all", "--mouse", "--color=auto"]).host == "unix:///x")
        #expect(try parse(["--color=auto"]).colorMode == nil)
        #expect(try parse(["--running", "--all"]).showStopped)
    }

    @Test("selects run modes", arguments: [
        (["-h"], RunMode.help), (["--help"], .help), (["-v"], .version), (["--version"], .version), (["--once"], .once),
        (["--list-themes"], .listThemes), (["--print-theme", "ansi"], .printTheme("ansi")), ([], .interactive)
    ])
    func modes(arguments: [String], expected: RunMode) throws {
        #expect(try parse(arguments).mode == expected)
    }

    @Test("rejects bad input with actionable messages", arguments: [
        ["--bogus"], ["-i"], ["-i", "0.1"], ["-i", "61"], ["-i", "fast"], ["--color", "rainbow"], ["--sort", "size"],
        ["--history", "5"], ["--history", "lots"], ["--host"], ["positional"]
    ])
    func errors(arguments: [String]) {
        #expect(throws: ConfigurationError.self) { try parse(arguments) }
    }

    @Test("unknown options point at --help")
    func unknownMessage() {
        do {
            _ = try parse(["--frobnicate"])
            Issue.record("expected an error")
        } catch {
            #expect(String(describing: error).contains("--help"))
            #expect(String(describing: error).contains("--frobnicate"))
        }
    }

    @Test("intervals accept seconds and milliseconds", arguments: [
        ("2", 2.0), ("0.25", 0.25), ("1.5s", 1.5), ("750ms", 0.75), ("60", 60.0)
    ])
    func intervals(text: String, seconds: Double) throws {
        #expect(try AppConfiguration.parseInterval(text) == seconds)
    }

    @Test("pre-scans --config before other flags")
    func configPath() throws {
        #expect(try AppConfiguration.configPathArgument(["-t", "x", "--config", "/a.conf"]) == "/a.conf")
        #expect(try AppConfiguration.configPathArgument(["--config=/b.conf"]) == "/b.conf")
        #expect(try AppConfiguration.configPathArgument(["-c", "/c.conf"]) == "/c.conf")
        #expect(try AppConfiguration.configPathArgument(["--once"]) == nil)
        #expect(throws: ConfigurationError.self) { try AppConfiguration.configPathArgument(["--config"]) }
    }

    @Test("usage documents every flag")
    func usage() {
        for flag in ["--host", "--interval", "--theme", "--color", "--running", "--sort", "--no-mouse", "--history",
                     "--config", "--once", "--list-themes", "--print-theme", "--help", "--version"] {
            #expect(AppConfiguration.usage.contains(flag), "usage is missing \(flag)")
        }
    }
}

@Suite("Config file")
struct ConfigFileTests {
    @Test("applies settings with comments, quotes and dashed keys")
    func apply() throws {
        var configuration = AppConfiguration()
        try configuration.apply(configFile: """
        # dtop settings
        ; another comment
        host = "unix:///custom.sock"
        refresh-interval = 1s
        theme = daylight
        color = 16
        mouse = off
        show_stopped = no
        sort = name
        sort_descending = false
        history = 600

        """)
        #expect(configuration.host == "unix:///custom.sock")
        #expect(configuration.refreshInterval == 1)
        #expect(configuration.themeName == "daylight")
        #expect(configuration.colorMode == .basic16)
        #expect(!configuration.mouse)
        #expect(!configuration.showStopped)
        #expect(configuration.sortKey == .name)
        #expect(configuration.sortDescending == false)
        #expect(configuration.historySize == 600)
    }

    @Test("sort direction follows the key unless set explicitly")
    func sortDirection() throws {
        var byName = AppConfiguration()
        try byName.apply(arguments: ["--sort", "name"])
        #expect(byName.sortDescending == nil)
        #expect(!AppState(configuration: byName).sortDescending, "names sort A→Z")
        var byMemory = AppConfiguration()
        try byMemory.apply(arguments: ["--sort", "memory"])
        #expect(AppState(configuration: byMemory).sortDescending, "biggest consumers first")
        #expect(AppState(configuration: AppConfiguration()).sortDescending, "default cpu sort is descending")
        var explicit = AppConfiguration()
        try explicit.apply(configFile: "sort_descending = true\nsort = name")
        #expect(AppState(configuration: explicit).sortDescending, "an explicit direction wins regardless of order")
    }

    @Test("command-line flags override the file")
    func precedence() throws {
        var configuration = AppConfiguration()
        try configuration.apply(configFile: "theme = daylight\ninterval = 5")
        try configuration.apply(arguments: ["--theme", "ansi"])
        #expect(configuration.themeName == "ansi")
        #expect(configuration.refreshInterval == 5)
    }

    @Test("errors carry the line number", arguments: [
        ("nonsense", 1), ("theme = x\nwat = 1", 2), ("\n\nmouse = maybe", 3), ("interval = 0", 1), ("theme =", 1),
        ("color = loud", 1), ("sort = random", 1)
    ])
    func errors(text: String, line: Int) {
        var configuration = AppConfiguration()
        do {
            try configuration.apply(configFile: text)
            Issue.record("expected an error for \(text.debugDescription)")
        } catch let error as ConfigurationError {
            #expect(error.message.hasPrefix("dtop.conf line \(line):"))
        } catch {
            Issue.record("unexpected error \(error)")
        }
    }

    @Test("an empty host clears it and auto color resets detection")
    func clearing() throws {
        var configuration = AppConfiguration()
        configuration.host = "x"
        configuration.colorMode = ColorMode.none
        try configuration.apply(configFile: "host =\ncolor = auto")
        #expect(configuration.host == nil)
        #expect(configuration.colorMode == nil)
    }

    @Test("booleans accept common spellings")
    func booleans() throws {
        for value in ["1", "true", "YES", "on"] { #expect(try AppConfiguration.parseBool(value, key: "k")) }
        for value in ["0", "false", "No", "OFF"] { #expect(try !AppConfiguration.parseBool(value, key: "k")) }
        #expect(throws: ConfigurationError.self) { try AppConfiguration.parseBool("perhaps", key: "k") }
    }
}

@Suite("Config locations")
struct ConfigLocationTests {
    @Test("POSIX uses XDG_CONFIG_HOME, then ~/.config")
    func posix() {
        #expect(AppConfiguration.configDirectory(environment: ["XDG_CONFIG_HOME": "/xdg"], platform: .linux, homeDirectory: "/h")
            == "/xdg/dtop")
        #expect(AppConfiguration.configDirectory(environment: ["XDG_CONFIG_HOME": "relative"], platform: .linux,
                                                 homeDirectory: "/h") == "/h/.config/dtop")
        #expect(AppConfiguration.configDirectory(environment: [:], platform: .macOS, homeDirectory: "/Users/me")
            == "/Users/me/.config/dtop")
        #expect(AppConfiguration.configDirectory(environment: [:], platform: .linux, homeDirectory: nil) == nil)
    }

    @Test("Windows uses APPDATA, then USERPROFILE")
    func windows() {
        #expect(AppConfiguration.configDirectory(environment: ["APPDATA": #"C:\Users\me\AppData\Roaming"#], platform: .windows,
                                                 homeDirectory: nil) == #"C:\Users\me\AppData\Roaming\dtop"#)
        #expect(AppConfiguration.configDirectory(environment: ["USERPROFILE": #"C:\Users\me"#], platform: .windows,
                                                 homeDirectory: nil) == #"C:\Users\me\AppData\Roaming\dtop"#)
        #expect(AppConfiguration.configDirectory(environment: [:], platform: .windows, homeDirectory: nil) == nil)
    }
}

@Suite("Theme loader")
struct ThemeLoaderTests {
    let files = [
        "/cfg/themes/ocean.theme": "name = ocean\naccent = #0000ff",
        "/elsewhere/custom.theme": "inherits = ansi\naccent = red",
        "/cfg/themes/broken.theme": "accent = ???"
    ]

    func read(_ path: String) -> String? { files[path] }

    @Test("resolves built-ins, the themes directory and explicit paths")
    func load() throws {
        let loader = ThemeLoader(themesDirectory: "/cfg/themes", platform: .linux)
        #expect(try loader.load("midnight", readFile: read) == .midnight)
        #expect(try loader.load("ocean", readFile: read)[.accent] == .rgb(0, 0, 255))
        let custom = try loader.load("/elsewhere/custom.theme", readFile: read)
        #expect(custom.name == "custom")
        #expect(custom[.accent] == .ansi(1))
        #expect(custom[.memory] == Theme.ansi[.memory])
    }

    @Test("errors name the file or list the alternatives")
    func errors() {
        let loader = ThemeLoader(themesDirectory: "/cfg/themes", platform: .linux)
        do {
            _ = try loader.load("broken", readFile: read)
            Issue.record("expected an error")
        } catch {
            #expect(String(describing: error).contains("/cfg/themes/broken.theme"))
            #expect(String(describing: error).contains("line 1"))
        }
        do {
            _ = try loader.load("missing", readFile: read)
            Issue.record("expected an error")
        } catch {
            #expect(String(describing: error).contains("harbor"))
            #expect(String(describing: error).contains("/cfg/themes"))
        }
    }

    @Test("lists built-ins followed by custom theme files")
    func available() {
        let loader = ThemeLoader(themesDirectory: "/cfg/themes", platform: .linux)
        let names = loader.availableNames { _ in ["zeta.theme", "ocean.theme", "notes.txt", "harbor.theme"] }
        #expect(names == Theme.builtIn.map(\.name) + ["ocean", "zeta"])
        #expect(ThemeLoader(themesDirectory: nil).availableNames { _ in ["x.theme"] } == Theme.builtIn.map(\.name))
    }

    @Test("base names strip directories and the extension")
    func baseName() {
        #expect(ThemeLoader.baseName(of: "/a/b/c.theme", platform: .linux) == "c")
        #expect(ThemeLoader.baseName(of: #"C:\t\x.theme"#, platform: .windows) == "x")
        #expect(ThemeLoader.baseName(of: "plain", platform: .macOS) == "plain")
    }
}

@Suite("App profile")
struct AppProfileTests {
    @Test("profile keeps the docker management focus")
    func profile() {
        let profile = AppProfile.current
        #expect(profile.name == "dtop")
        #expect(!profile.version.isEmpty)
        #expect(profile.capabilities.contains("Docker resource management"))
        #expect(profile.capabilities.contains("Color-coded container and compose views"))
    }
}
