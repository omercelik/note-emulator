import Foundation

enum AccessPointConnectionState: Equatable {
    case idle, connecting, connected
    case failed(String)
}

enum AccessPointProbeResult: Equatable {
    case reachable, httpError(Int), unreachable
}

struct AccessPointReachability {
    static func check(_ url: URL) async -> AccessPointProbeResult {
        let config = URLSessionConfiguration.ephemeral
        config.connectionProxyDictionary = [:]
        config.timeoutIntervalForRequest = 2
        config.timeoutIntervalForResource = 2
        let session = URLSession(configuration: config)
        defer { session.invalidateAndCancel() }
        return await check(url, session: session)
    }

    static func check(_ url: URL, session: URLSession) async -> AccessPointProbeResult {
        do {
            var request = URLRequest(url: url)
            request.cachePolicy = .reloadIgnoringLocalCacheData
            let (_, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse,
                  response.url?.host == url.host else { return .unreachable }
            return (200...299).contains(http.statusCode) ? .reachable : .httpError(http.statusCode)
        } catch { return .unreachable }
    }
}
