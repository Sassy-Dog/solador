//! Per-model token prices. Port of
//! `ModelPricing`.
//!
//! Cost is **computed and tested but never displayed**: the account is
//! subscription-based, so a dollar figure on the panel would be a number that
//! matches nothing the user is billed. It stays here because per-model pricing
//! is the only way to compare a window that mixes models, and because dropping
//! the arithmetic would make it unrecoverable later.

/// Per-model token prices in USD per 1,000,000 tokens.
///
/// These are **approximate** public list prices and are intended to be editable
/// — Anthropic pricing changes over time and the local logs carry no price
/// data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPricing {
    /// USD per 1M input tokens.
    pub input: f64,
    /// USD per 1M output tokens.
    pub output: f64,
    /// USD per 1M cache-write (`cache_creation`) tokens.
    pub cache_write: f64,
    /// USD per 1M cache-read tokens.
    pub cache_read: f64,
}

impl ModelPricing {
    pub const OPUS: ModelPricing = ModelPricing {
        input: 15.0,
        output: 75.0,
        cache_write: 18.75,
        cache_read: 1.5,
    };
    pub const SONNET: ModelPricing = ModelPricing {
        input: 3.0,
        output: 15.0,
        cache_write: 3.75,
        cache_read: 0.30,
    };
    pub const HAIKU: ModelPricing = ModelPricing {
        input: 1.0,
        output: 5.0,
        cache_write: 1.25,
        cache_read: 0.10,
    };

    /// Resolves pricing for a model id by case-insensitive substring
    /// (`opus` / `sonnet` / `haiku`). Unknown ids price as Sonnet.
    ///
    /// The fallback is deliberate rather than an `Option`: an unrecognised id
    /// is almost always a model newer than this table, and the panel never
    /// renders the figure anyway. Refusing to price it would lose the record's
    /// tokens from the model breakdown, which *is* displayed.
    #[must_use]
    pub fn for_model(model: &str) -> ModelPricing {
        let m = model.to_lowercase();
        if m.contains("opus") {
            return ModelPricing::OPUS;
        }
        if m.contains("sonnet") {
            return ModelPricing::SONNET;
        }
        if m.contains("haiku") {
            return ModelPricing::HAIKU;
        }
        ModelPricing::SONNET
    }

    /// Cost of a single record's token counts under this price table.
    #[must_use]
    pub fn cost(&self, input: u64, output: u64, cache_write: u64, cache_read: u64) -> f64 {
        per_million(input) * self.input
            + per_million(output) * self.output
            + per_million(cache_write) * self.cache_write
            + per_million(cache_read) * self.cache_read
    }
}

/// Token count as millions of tokens. `as f64` is lossless for any token count
/// a log can hold (u64 stays exact below 2^53).
fn per_million(tokens: u64) -> f64 {
    tokens as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_a_model_family_by_substring_case_insensitively() {
        assert_eq!(
            ModelPricing::for_model("claude-opus-4-7"),
            ModelPricing::OPUS
        );
        assert_eq!(
            ModelPricing::for_model("claude-sonnet-4-5"),
            ModelPricing::SONNET
        );
        assert_eq!(
            ModelPricing::for_model("claude-3-5-haiku-20241022"),
            ModelPricing::HAIKU
        );
        assert_eq!(ModelPricing::for_model("CLAUDE-OPUS-4"), ModelPricing::OPUS);
    }

    /// Twin of `ClaudeUsageAggregatorTests.testUnknownModelPricedAsSonnet`.
    #[test]
    fn an_unknown_model_prices_as_sonnet() {
        assert_eq!(
            ModelPricing::for_model("some-future-model"),
            ModelPricing::SONNET
        );
        assert_eq!(ModelPricing::for_model(""), ModelPricing::SONNET);
    }

    /// Twin of `testCostComputedFromPricingTablePerModel`: 1M of each kind on
    /// Opus is 15 + 75 + 18.75 + 1.5.
    #[test]
    fn costs_one_million_of_each_kind_at_the_table_rate() {
        let cost = ModelPricing::OPUS.cost(1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert!((cost - 110.25).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn no_tokens_cost_nothing() {
        assert_eq!(ModelPricing::SONNET.cost(0, 0, 0, 0), 0.0);
    }
}
