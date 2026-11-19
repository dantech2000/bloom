import Foundation
import Network

/// A TCP relay in front of the mock, for the offline test: the app talks
/// to the relay's port. `stop` refuses new connections and cuts the open
/// ones (the socket among them), as a server that went away does;
/// `start` brings the server back. The mock itself keeps running for the
/// other tests. The first start takes a free port; a start after a stop
/// listens on the same one, so the app finds the server where it was.
nonisolated final class Relay: @unchecked Sendable {
    private let target: NWEndpoint.Port
    private let queue = DispatchQueue(label: "relay")
    private var listener: NWListener?
    private var connections: [NWConnection] = []
    private var bound: NWEndpoint.Port = .any

    init(to target: UInt16) {
        self.target = NWEndpoint.Port(rawValue: target) ?? .any
    }

    /// The port the relay listens on (0 before the first start).
    var port: UInt16 { queue.sync { bound.rawValue } }

    /// True once the relay listens.
    @discardableResult
    func start() -> Bool {
        let parameters = NWParameters.tcp
        parameters.allowLocalEndpointReuse = true
        let port = queue.sync { bound }
        guard let listener = try? NWListener(using: parameters, on: port) else { return false }
        let ready = DispatchSemaphore(value: 0)
        nonisolated(unsafe) var listening = false
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready: listening = true; ready.signal()
            case .failed, .cancelled: ready.signal()
            default: break
            }
        }
        listener.newConnectionHandler = { [weak self] client in self?.accept(client) }
        listener.start(queue: queue)
        _ = ready.wait(timeout: .now() + 5)
        queue.sync {
            self.listener = listener
            if listening, let port = listener.port { self.bound = port }
        }
        return listening
    }

    func stop() {
        queue.sync {
            listener?.cancel()
            listener = nil
            connections.forEach { $0.cancel() }
            connections = []
        }
    }

    /// On the relay's queue.
    private func accept(_ client: NWConnection) {
        let server = NWConnection(host: "127.0.0.1", port: target, using: .tcp)
        connections += [client, server]
        client.start(queue: queue)
        server.start(queue: queue)
        pipe(client, to: server)
        pipe(server, to: client)
    }

    private func pipe(_ from: NWConnection, to: NWConnection) {
        from.receive(minimumIncompleteLength: 1, maximumLength: 65_536) { [weak self] data, _, done, error in
            if let data, !data.isEmpty {
                to.send(content: data, completion: .contentProcessed { _ in })
            }
            if done || error != nil {
                // One side ended: the other ends after what it was sent.
                to.send(content: nil, contentContext: .finalMessage, isComplete: true, completion: .contentProcessed { _ in
                    to.cancel()
                    from.cancel()
                })
                return
            }
            self?.pipe(from, to: to)
        }
    }
}
