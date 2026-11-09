public struct AppProfile: Sendable {
    public let name: String
    public let version: String
    public let tagline: String
    public let capabilities: [String]

    public static let current = AppProfile(
        name: "dtop",
        version: BuildVersion.current,
        tagline: "Docker-first terminal observability and control",
        capabilities: [
            "Minimal terminal UI",
            "Mouse interaction",
            "Low resource footprint",
            "Real-time monitoring",
            "Color-coded container and compose views",
            "Process relationship tree",
            "Theme customization",
            "Docker resource management"
        ]
    )
}
