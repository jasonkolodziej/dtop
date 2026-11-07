/// Fixed-capacity FIFO that overwrites the oldest element. Used for metric history so memory use
/// stays constant no matter how long dtop runs.
public struct RingBuffer<Element: Sendable>: Sendable, Sequence {
    public let capacity: Int
    private var storage: [Element?]
    private var head = 0
    public private(set) var count = 0

    public init(capacity: Int) {
        precondition(capacity > 0, "RingBuffer capacity must be positive")
        self.capacity = capacity
        storage = Array(repeating: nil, count: capacity)
    }

    public var isEmpty: Bool { count == 0 }
    public var isFull: Bool { count == capacity }

    public mutating func append(_ element: Element) {
        storage[(head + count) % capacity] = element
        if count < capacity {
            count += 1
        } else {
            head = (head + 1) % capacity
        }
    }

    public mutating func removeAll() {
        storage = Array(repeating: nil, count: capacity)
        head = 0
        count = 0
    }

    /// Oldest-first element access.
    public subscript(index: Int) -> Element {
        precondition(index >= 0 && index < count, "RingBuffer index out of range")
        return storage[(head + index) % capacity]!
    }

    public var last: Element? { count == 0 ? nil : self[count - 1] }
    public var first: Element? { count == 0 ? nil : self[0] }

    /// Oldest-first array copy.
    public var elements: [Element] { (0..<count).map { self[$0] } }

    /// The newest `n` elements, oldest first.
    public func suffix(_ n: Int) -> [Element] {
        let take = Swift.max(0, Swift.min(n, count))
        return (count - take..<count).map { self[$0] }
    }

    public func makeIterator() -> IndexingIterator<[Element]> {
        elements.makeIterator()
    }
}
