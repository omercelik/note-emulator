import Foundation

/// In-memory values for running control sockets. A new runtime has a new socket;
/// quitting the app drops every value. Never writes passwords to disk.
@MainActor
enum AccessPointPasswordCache {
    private static var values: [String: String] = [:]

    static func password(for socket: String) -> String { values[socket] ?? "" }
    static func remember(_ password: String, for socket: String) { values[socket] = password }
}
