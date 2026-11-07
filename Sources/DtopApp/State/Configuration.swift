import DtopCore
import DtopUI

public enum RunMode: Sendable, Equatable {
    case interactive
    case once
    case help
    case version
    case listThemes
    case printTheme(String)
}

public struct ConfigurationError: Error, Equatable, CustomStringConvertible {
    public var message: String

    public init(_ message: String) {
        self.message = message
    }

    public var description: String { message }
}

/// Everything that can be set from the config file or the command line.
public struct AppConfiguration: Sendable, Equatable {
    public var host: String?
    public var refreshInterval: Double = 2
    public var themeName: String = "harbor"
    public var colorMode: ColorMode?
    public var mouse = true
    public var showStopped = true
    public var sortKey: SortKey = .cpu
    public var sortDescending = true
    public var historySize = 240
    public var configFilePath: String?
    public var mode: RunMode = .interactive

    public init() {}

    public static let intervalRange: ClosedRange<Double> = 0.25...60
    public static let historyRange: ClosedRange<Int> = 30...3600

    /// Applies `key = value` lines from `dtop.conf`. Unknown keys are errors so typos surface.
    public mutating func apply(configFile text: String) throws {
        for (offset, rawLine) in text.split(separator: "\n", omittingEmptySubsequences: false).enumerated() {
            let line = String(rawLine).trimmingSpaces()
            if line.isEmpty || line.hasPrefix("#") || line.hasPrefix(";") { continue }
            guard let equals = line.firstIndex(of: "=") else {
                throw ConfigurationError("dtop.conf line \(offset + 1): expected 'key = value', found '\(line)'")
            }
            let key = String(line[..<equals]).trimmingSpaces().lowercased().replacingDashes()
            var value = String(line[line.index(after: equals)...]).trimmingSpaces()
            if value.count >= 2, value.hasPrefix("\""), value.hasSuffix("\"") { value = String(value.dropFirst().dropLast()) }
            do {
                try apply(key: key, value: value)
            } catch let error as ConfigurationError {
                throw ConfigurationError("dtop.conf line \(offset + 1): \(error.message)")
            }
        }
    }

    mutating func apply(key: String, value: String) throws {
        switch key {
        case "host", "docker_host":
            host = value.isEmpty ? nil : value
        case "interval", "refresh_interval":
            refreshInterval = try Self.parseInterval(value)
        case "theme":
            guard !value.isEmpty else { throw ConfigurationError("theme must not be empty") }
            themeName = value
        case "color", "color_mode":
            if value.lowercased() == "auto" {
                colorMode = nil
            } else {
                guard let mode = ColorMode.parse(value) else {
                    throw ConfigurationError("invalid color mode '\(value)' — use auto, truecolor, 256, 16 or none")
                }
                colorMode = mode
            }
        case "mouse":
            mouse = try Self.parseBool(value, key: key)
        case "show_stopped", "all":
            showStopped = try Self.parseBool(value, key: key)
        case "sort":
            guard let parsed = SortKey(rawValue: value.lowercased()) else {
                throw ConfigurationError("invalid sort '\(value)' — use one of \(SortKey.allCases.map(\.rawValue).joined(separator: ", "))")
            }
            sortKey = parsed
        case "sort_descending", "descending":
            sortDescending = try Self.parseBool(value, key: key)
        case "history", "history_size":
            guard let size = Int(value), Self.historyRange.contains(size) else {
                throw ConfigurationError("history must be a whole number between \(Self.historyRange.lowerBound) and \(Self.historyRange.upperBound)")
            }
            historySize = size
        default:
            throw ConfigurationError("unknown setting '\(key)'")
        }
    }

    static func parseInterval(_ value: String) throws -> Double {
        var text = value.lowercased()
        var multiplier = 1.0
        if text.hasSuffix("ms") {
            text.removeLast(2)
            multiplier = 0.001
        } else if text.hasSuffix("s") {
            text.removeLast()
        }
        guard let number = Double(text), number.isFinite else {
            throw ConfigurationError("invalid interval '\(value)' — use seconds such as 2, 0.5 or 500ms")
        }
        let seconds = number * multiplier
        guard intervalRange.contains(seconds) else {
            throw ConfigurationError("interval must be between \(intervalRange.lowerBound)s and \(Int(intervalRange.upperBound))s (got \(value))")
        }
        return seconds
    }

    static func parseBool(_ value: String, key: String) throws -> Bool {
        switch value.lowercased() {
        case "1", "true", "yes", "on": return true
        case "0", "false", "no", "off": return false
        default: throw ConfigurationError("\(key) must be true or false (got '\(value)')")
        }
    }

    // MARK: - Command line

    public static let usage = """
    dtop — Docker-first terminal observability and control

    USAGE:
      dtop [options]

    OPTIONS:
      -H, --host <endpoint>     Docker endpoint (unix:///var/run/docker.sock, npipe:////./pipe/docker_engine,
                                tcp://host:2375). Defaults to DOCKER_HOST, then well-known sockets.
      -i, --interval <seconds>  Refresh interval, 0.25-60 (default 2; accepts 500ms)
      -t, --theme <name|file>   Built-in theme name or path to a .theme file (default harbor)
          --color <mode>        auto, truecolor, 256, 16 or none (default auto; honours NO_COLOR)
          --running             Only list running containers
          --sort <key>          Initial sort: name, cpu, memory, status, created, image
          --no-mouse            Disable mouse reporting
          --history <samples>   Samples kept per graph (30-3600, default 240)
      -c, --config <file>       Read settings from this file instead of the default location
          --once                Print one plain-text snapshot and exit (scriptable)
          --list-themes         List built-in themes
          --print-theme <name>  Print a built-in theme in theme-file format (a starting point for your own)
      -h, --help                Show this help
      -v, --version             Show version

    KEYS (press ? inside dtop for the full list):
      ↑/↓ j/k move · ←/→ Tab switch views · / filter · s sort · Enter details · q quit
    """

    /// Pre-scans for `--config` so the file can be applied before other flags override it.
    public static func configPathArgument(_ arguments: [String]) throws -> String? {
        var index = 0
        while index < arguments.count {
            let argument = arguments[index]
            if argument == "-c" || argument == "--config" {
                guard index + 1 < arguments.count else { throw ConfigurationError("\(argument) requires a file path") }
                return arguments[index + 1]
            }
            if let value = argument.valueAfterPrefix("--config=") { return value }
            index += 1
        }
        return nil
    }

    /// Applies command-line flags (arguments exclude the program name).
    public mutating func apply(arguments: [String]) throws {
        var index = 0
        func nextValue(_ flag: String) throws -> String {
            guard index + 1 < arguments.count else { throw ConfigurationError("\(flag) requires a value — see dtop --help") }
            index += 1
            return arguments[index]
        }
        while index < arguments.count {
            var argument = arguments[index]
            var inlineValue: String?
            if argument.hasPrefix("--"), let equals = argument.firstIndex(of: "=") {
                inlineValue = String(argument[argument.index(after: equals)...])
                argument = String(argument[..<equals])
            }
            func value() throws -> String {
                if let inlineValue { return inlineValue }
                return try nextValue(argument)
            }
            switch argument {
            case "-h", "--help":
                mode = .help
            case "-v", "--version":
                mode = .version
            case "--once":
                mode = .once
            case "--list-themes":
                mode = .listThemes
            case "--print-theme":
                mode = .printTheme(try value())
            case "-H", "--host":
                host = try value()
            case "-i", "--interval":
                refreshInterval = try Self.parseInterval(try value())
            case "-t", "--theme":
                themeName = try value()
            case "--color":
                try apply(key: "color", value: try value())
            case "--running":
                showStopped = false
            case "--all":
                showStopped = true
            case "--sort":
                try apply(key: "sort", value: try value())
            case "--no-mouse":
                mouse = false
            case "--mouse":
                mouse = true
            case "--history":
                try apply(key: "history", value: try value())
            case "-c", "--config":
                configFilePath = try value()
            default:
                throw ConfigurationError("unknown option '\(arguments[index])' — run dtop --help for usage")
            }
            index += 1
        }
    }

    /// Default location of `dtop.conf` and the `themes/` directory:
    /// `$XDG_CONFIG_HOME/dtop`, `~/.config/dtop`, or `%APPDATA%\dtop` on Windows.
    public static func configDirectory(environment: [String: String], platform: Platform, homeDirectory: String?) -> String? {
        if platform == .windows {
            if let appData = environment["APPDATA"], !appData.isEmpty {
                return joinPath([appData, "dtop"], platform: platform)
            }
            if let profile = environment["USERPROFILE"], !profile.isEmpty {
                return joinPath([profile, "AppData", "Roaming", "dtop"], platform: platform)
            }
            return nil
        }
        if let xdg = environment["XDG_CONFIG_HOME"], xdg.hasPrefix("/") {
            return joinPath([xdg, "dtop"], platform: platform)
        }
        guard let home = homeDirectory, !home.isEmpty else { return nil }
        return joinPath([home, ".config", "dtop"], platform: platform)
    }
}

extension String {
    func trimmingSpaces() -> String {
        var view = Substring(self)
        while let first = view.first, first.isWhitespace { view = view.dropFirst() }
        while let last = view.last, last.isWhitespace { view = view.dropLast() }
        return String(view)
    }

    func replacingDashes() -> String {
        String(map { $0 == "-" ? "_" : $0 })
    }

    func valueAfterPrefix(_ prefix: String) -> String? {
        hasPrefix(prefix) ? String(dropFirst(prefix.count)) : nil
    }
}
