// swift-tools-version: 6.0
import PackageDescription

// dtop deliberately has zero third-party dependencies: everything (HTTP over Unix sockets and
// Windows named pipes, terminal rendering, input parsing) is built on Foundation and the
// platform C libraries so the same sources build on macOS, Linux and Windows.
let package = Package(
    name: "dtop",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(name: "dtop", targets: ["dtop"]),
        .library(name: "DtopCore", targets: ["DtopCore"]),
        .library(name: "DtopUI", targets: ["DtopUI"]),
        .library(name: "DtopApp", targets: ["DtopApp"])
    ],
    targets: [
        // Docker Engine API client, transports, domain models and metrics math.
        .target(name: "DtopCore"),
        // Terminal primitives: colors, screen buffer, diff renderer, widgets, themes, input parsing.
        .target(name: "DtopUI"),
        // Application state, reducer, views and the runtime loop.
        .target(name: "DtopApp", dependencies: ["DtopCore", "DtopUI"]),
        .executableTarget(name: "dtop", dependencies: ["DtopApp"]),
        .testTarget(name: "DtopCoreTests", dependencies: ["DtopCore"]),
        .testTarget(name: "DtopUITests", dependencies: ["DtopUI"]),
        .testTarget(name: "DtopAppTests", dependencies: ["DtopApp", "DtopCore", "DtopUI"])
    ]
)
