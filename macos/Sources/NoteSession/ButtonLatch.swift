/// Buttons this window is holding down. Focus loss and disconnect release all of them.
public struct ButtonLatch: Equatable, Sendable {
    public private(set) var held: Set<String> = []

    public init() {}

    /// True when this is a new press and `button.down` should be sent.
    public mutating func down(_ id: String) -> Bool {
        held.insert(id).inserted
    }

    /// True when the button was held and `button.up` should be sent.
    public mutating func up(_ id: String) -> Bool {
        held.remove(id) != nil
    }

    /// Every held button, in a stable order, then the latch is empty.
    public mutating func releaseAll() -> [String] {
        let ids = held.sorted()
        held.removeAll()
        return ids
    }
}
