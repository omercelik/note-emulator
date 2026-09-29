// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "NoteEmulator",
    platforms: [.macOS(.v15)],
    products: [
        .executable(name: "NoteEmulator", targets: ["NoteEmulator"]),
        .library(name: "NoteProtocol", targets: ["NoteProtocol"]),
    ],
    targets: [
        .target(name: "NoteProtocol"),
        .target(name: "NoteSession", dependencies: ["NoteProtocol"]),
        .target(name: "NoteMedia"),
        .executableTarget(name: "NoteEmulator", dependencies: ["NoteProtocol", "NoteSession", "NoteMedia"]),
        .testTarget(
            name: "NoteProtocolTests",
            dependencies: ["NoteProtocol"],
            resources: [.copy("Fixtures")]
        ),
        .testTarget(name: "NoteSessionTests", dependencies: ["NoteSession"]),
        .testTarget(name: "NoteMediaTests", dependencies: ["NoteMedia"]),
        .testTarget(name: "NoteEmulatorTests", dependencies: ["NoteEmulator"]),
    ]
)
