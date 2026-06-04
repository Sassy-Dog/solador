import Foundation

/// A fixed-size circular buffer optimized for performance monitoring history
///
/// This data structure provides O(1) append operations and maintains a fixed maximum size,
/// making it ideal for storing time-series data like CPU usage, memory usage, etc.
///
/// Unlike using `Array.append()` + `Array.removeFirst()` which is O(n), this buffer
/// uses a ring buffer approach with O(1) operations for both reads and writes.
public struct CircularBuffer<Element> {
    private var storage: [Element]
    private var head: Int = 0
    private var tail: Int = 0
    public private(set) var count: Int = 0
    public let capacity: Int

    /// Creates a new circular buffer with the specified capacity
    /// - Parameter capacity: The maximum number of elements the buffer can hold
    public init(capacity: Int) {
        precondition(capacity > 0, "Capacity must be greater than zero")
        self.capacity = capacity
        storage = []
        storage.reserveCapacity(capacity)
    }

    /// Creates a circular buffer initialized with values
    /// - Parameters:
    ///   - repeating: The value to repeat
    ///   - count: The number of times to repeat the value (becomes the capacity)
    public init(repeating value: Element, count: Int) {
        precondition(count > 0, "Count must be greater than zero")
        capacity = count
        storage = Array(repeating: value, count: count)
        self.count = count
        head = 0
        tail = count > 0 ? count - 1 : 0
    }

    /// Appends an element to the buffer
    ///
    /// If the buffer is at capacity, this overwrites the oldest element.
    /// Time complexity: O(1)
    ///
    /// - Parameter element: The element to append
    public mutating func append(_ element: Element) {
        if count < capacity {
            // Buffer not full yet
            storage.append(element)
            tail = storage.count - 1
            count += 1
        } else {
            // Buffer full, overwrite oldest
            storage[head] = element
            head = (head + 1) % capacity
            tail = (tail + 1) % capacity
        }
    }

    /// Accesses the element at the specified index
    ///
    /// - Parameter index: The position of the element (0 is the oldest element)
    /// - Returns: The element at the specified index
    public subscript(index: Int) -> Element {
        get {
            precondition(index >= 0 && index < count, "Index out of bounds")
            let actualIndex = (head + index) % capacity
            return storage[actualIndex]
        }
        set {
            precondition(index >= 0 && index < count, "Index out of bounds")
            let actualIndex = (head + index) % capacity
            storage[actualIndex] = newValue
        }
    }

    /// Returns all elements as an array in order (oldest to newest)
    ///
    /// Time complexity: O(n)
    ///
    /// - Returns: Array of elements from oldest to newest
    public func toArray() -> [Element] {
        guard count > 0 else { return [] }

        var result: [Element] = []
        result.reserveCapacity(count)

        for i in 0 ..< count {
            result.append(self[i])
        }

        return result
    }

    /// Clears all elements from the buffer
    public mutating func removeAll() {
        storage.removeAll(keepingCapacity: true)
        head = 0
        tail = 0
        count = 0
    }

    /// Returns the first (oldest) element
    public var first: Element? {
        count > 0 ? self[0] : nil
    }

    /// Returns the last (newest) element
    public var last: Element? {
        count > 0 ? self[count - 1] : nil
    }

    /// Returns whether the buffer is empty
    public var isEmpty: Bool {
        count == 0
    }

    /// Returns whether the buffer is full
    public var isFull: Bool {
        count == capacity
    }
}

// MARK: - Sequence Conformance

extension CircularBuffer: Sequence {
    public func makeIterator() -> CircularBufferIterator<Element> {
        CircularBufferIterator(buffer: self)
    }
}

public struct CircularBufferIterator<Element>: IteratorProtocol {
    private let buffer: CircularBuffer<Element>
    private var currentIndex: Int = 0

    init(buffer: CircularBuffer<Element>) {
        self.buffer = buffer
    }

    public mutating func next() -> Element? {
        guard currentIndex < buffer.count else { return nil }
        let element = buffer[currentIndex]
        currentIndex += 1
        return element
    }
}

// MARK: - Collection Conformance

extension CircularBuffer: RandomAccessCollection {
    public var startIndex: Int {
        0
    }

    public var endIndex: Int {
        count
    }

    public func index(after i: Int) -> Int {
        i + 1
    }

    public func index(before i: Int) -> Int {
        i - 1
    }
}

// MARK: - CustomStringConvertible

extension CircularBuffer: CustomStringConvertible {
    public var description: String {
        "CircularBuffer(capacity: \(capacity), count: \(count), elements: \(toArray()))"
    }
}

// MARK: - Equatable (when Element is Equatable)

extension CircularBuffer: Equatable where Element: Equatable {
    public static func == (lhs: CircularBuffer<Element>, rhs: CircularBuffer<Element>) -> Bool {
        lhs.capacity == rhs.capacity && lhs.toArray() == rhs.toArray()
    }
}
