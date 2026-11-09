import DtopCore

/// Decides what to do with the followed log stream when the selection, pane, host or the
/// stream itself changes. Pure, so the rules are tested without a terminal.
public enum LogStreamPolicy {
    public struct Target: Sendable, Equatable {
        public var host: Int
        public var containerID: String
        /// Whether the container is running (only running containers produce new lines).
        public var isActive: Bool

        public init(host: Int, containerID: String, isActive: Bool) {
            self.host = host
            self.containerID = containerID
            self.isActive = isActive
        }
    }

    public enum Decision: Sendable, Equatable {
        /// The open stream already shows the right container.
        case keep
        /// Close any open stream and start one for the target.
        case start
        /// Close any open stream and leave it closed.
        case stop
        /// Leave things as they are (an ended stream, or a reconnect already scheduled).
        case wait
    }

    /// Lines of a resumed stream that are new. `since` is inclusive, so lines at or before the
    /// last one already shown are dropped; lines without a timestamp are kept.
    public static func newLines(_ batch: [LogLine], after last: Double?) -> [LogLine] {
        guard let last else { return batch }
        return batch.filter { ($0.timestamp ?? .infinity) > last }
    }

    /// The logs pane's scroll-back after `appended` lines arrive while the user is scrolled back:
    /// it grows so the same lines stay on screen, but never past the oldest line kept.
    public static func scrollBack(_ current: Int, appended: Int, keptLines: Int) -> Int {
        max(0, min(current + appended, keptLines))
    }

    /// - Parameters:
    ///   - current: the stream that is open now, if any.
    ///   - wanted: the container the logs pane shows, or nil when no logs are visible.
    ///   - state: the shown container's last stream state (nil when its data is not loaded).
    ///   - reconnectPending: a reconnect is scheduled for the shown container.
    ///   - isReconnect: this call is that scheduled reconnect.
    public static func decide(current: (host: Int, containerID: String)?, wanted: Target?, state: LogStreamState?,
                              reconnectPending: Bool, isReconnect: Bool) -> Decision {
        guard let wanted else { return current == nil ? .wait : .stop }
        if let current, current.host == wanted.host, current.containerID == wanted.containerID { return .keep }
        if isReconnect { return .start }
        if current == nil, state != nil {
            // Same container as before: a stopped container's stream already delivered everything,
            // and a running one's reconnect is already on its way.
            if case .ended = state, !wanted.isActive { return .wait }
            if reconnectPending { return .wait }
        }
        return .start
    }
}
