import Foundation

/// Per-model token prices in USD per 1,000,000 tokens.
///
/// These are **approximate** public list prices and are intended to be editable
/// — Anthropic pricing changes over time and the local logs do not carry price
/// data. A model is matched by a case-insensitive substring of its id
/// (`opus` / `sonnet` / `haiku`); anything unrecognized falls back to Sonnet.
struct ModelPricing: Equatable {
    /// USD per 1M input tokens.
    let input: Double
    /// USD per 1M output tokens.
    let output: Double
    /// USD per 1M cache-write (cache_creation) tokens.
    let cacheWrite: Double
    /// USD per 1M cache-read tokens.
    let cacheRead: Double

    static let opus = ModelPricing(input: 15, output: 75, cacheWrite: 18.75, cacheRead: 1.5)
    static let sonnet = ModelPricing(input: 3, output: 15, cacheWrite: 3.75, cacheRead: 0.30)
    static let haiku = ModelPricing(input: 1, output: 5, cacheWrite: 1.25, cacheRead: 0.10)

    /// Resolves pricing for a model id. Unknown ids price as Sonnet.
    static func pricing(for model: String) -> ModelPricing {
        let m = model.lowercased()
        if m.contains("opus") { return .opus }
        if m.contains("sonnet") { return .sonnet }
        if m.contains("haiku") { return .haiku }
        return .sonnet
    }

    /// Cost of a single record's token counts under this price table.
    func cost(input: Int, output: Int, cacheWrite: Int, cacheRead: Int) -> Double {
        Double(input) / 1_000_000 * self.input
            + Double(output) / 1_000_000 * self.output
            + Double(cacheWrite) / 1_000_000 * self.cacheWrite
            + Double(cacheRead) / 1_000_000 * self.cacheRead
    }
}
