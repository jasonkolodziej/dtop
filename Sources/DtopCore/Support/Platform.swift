/// The operating system family dtop is running on. Kept as a value (instead of sprinkling `#if os`)
/// so platform-dependent decisions such as default Docker endpoints can be unit tested everywhere.
public enum Platform: String, Sendable, Equatable, CaseIterable {
    case macOS
    case linux
    case windows
    case other

    public static var current: Platform {
        #if os(macOS)
        return .macOS
        #elseif os(Linux)
        return .linux
        #elseif os(Windows)
        return .windows
        #else
        return .other
        #endif
    }

    public var pathSeparator: Character {
        self == .windows ? "\\" : "/"
    }
}

/// Joins path components with the platform separator without touching the file system.
public func joinPath(_ components: [String], platform: Platform = .current) -> String {
    let separator = platform.pathSeparator
    var result = ""
    for component in components where !component.isEmpty {
        if result.isEmpty {
            result = component
            continue
        }
        let trimmedResult = result.last == separator || (platform == .windows && result.last == "/")
            ? String(result.dropLast()) : result
        var trimmedComponent = Substring(component)
        while let first = trimmedComponent.first, first == separator || (platform == .windows && first == "/") {
            trimmedComponent = trimmedComponent.dropFirst()
        }
        result = trimmedResult + String(separator) + trimmedComponent
    }
    return result
}
