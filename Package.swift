// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "dtop",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(name: "dtop", targets: ["dtop"])
    ],
    targets: [
        .executableTarget(
            name: "dtop"
        ),
        .testTarget(
            name: "dtopTests",
            dependencies: ["dtop"]
        )
    ]
)
