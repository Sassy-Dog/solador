//! The pure fold. No clock, no network.

use std::collections::BTreeMap;

use crate::models::UsageItem;

/// How many models the tile names.
pub const TOP_MODELS: usize = 3;

/// One model's share of the month.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelUse {
    pub model: String,
    /// Gross quantity, in the report's unit.
    pub quantity: f64,
}

/// One report, folded.
#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    /// Gross quantity summed. `None` when any item lacks it.
    pub used: Option<f64>,
    /// Discount quantity summed (the included part). `None` when any item
    /// lacks it.
    pub included: Option<f64>,
    /// Net amount summed, in USD. `None` when any item lacks it.
    pub billed_usd: Option<f64>,
    /// The largest models by gross quantity, largest first, at most
    /// [`TOP_MODELS`]. Items with no model or no quantity are not attributed.
    pub top_models: Vec<ModelUse>,
}

/// `Some(sum)` only if every item carries the figure; an empty list sums to a
/// measured zero.
fn sum(items: &[UsageItem], pick: impl Fn(&UsageItem) -> Option<f64>) -> Option<f64> {
    items.iter().map(pick).sum::<Option<f64>>()
}

/// Fold a report's items.
#[must_use]
pub fn summarize(items: &[UsageItem]) -> Usage {
    let mut by_model: BTreeMap<&str, f64> = BTreeMap::new();
    for item in items {
        if let (Some(model), Some(quantity)) = (item.model.as_deref(), item.gross_quantity) {
            if !model.trim().is_empty() {
                *by_model.entry(model).or_insert(0.0) += quantity;
            }
        }
    }
    let mut top_models: Vec<ModelUse> = by_model
        .into_iter()
        .map(|(model, quantity)| ModelUse {
            model: model.to_owned(),
            quantity,
        })
        .collect();
    // `by_model` iterates by name, and the sort is stable, so ties stay by name.
    top_models.sort_by(|a, b| b.quantity.total_cmp(&a.quantity));
    top_models.truncate(TOP_MODELS);

    Usage {
        used: sum(items, |i| i.gross_quantity),
        included: sum(items, |i| i.discount_quantity),
        billed_usd: sum(items, |i| i.net_amount),
        top_models,
    }
}
