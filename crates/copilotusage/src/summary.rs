//! The pure fold. No clock, no network.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::models::UsageItem;

/// How many models the tile names, across the whole report.
pub const TOP_MODELS: usize = 3;

/// One model's share of the month, under the product that used it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelUse {
    pub model: String,
    /// Gross quantity, in the report's unit.
    pub quantity: f64,
}

/// One GitHub product's share of the month: the shape GitHub's own billing page
/// uses. The report counts every product billed in the unit, Copilot among them.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductUse {
    /// GitHub's `product` string, trimmed and otherwise untouched. `None` is
    /// the items whose `product` is absent or blank: never attributed to a
    /// product GitHub did not name.
    pub product: Option<String>,
    /// Gross quantity summed over the product's items. `None` when any of them
    /// lacks it.
    pub quantity: Option<f64>,
    /// This product's models among the report's [`TOP_MODELS`] largest, largest
    /// first. Items with no model or no quantity are not attributed.
    pub models: Vec<ModelUse>,
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
    /// Every product, so the lines add up to `used`: quantity descending, ties
    /// by name, unmeasured after measured, and the unnamed product always last.
    pub products: Vec<ProductUse>,
}

/// `Some(sum)` only if every item carries the figure; an empty list sums to a
/// measured zero.
fn sum(items: &[UsageItem], pick: impl Fn(&UsageItem) -> Option<f64>) -> Option<f64> {
    items.iter().map(pick).sum::<Option<f64>>()
}

/// A product's key: GitHub's string, trimmed; `None` when absent or blank.
fn product_key(item: &UsageItem) -> Option<&str> {
    item.product
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

/// Fold a report's items.
#[must_use]
pub fn summarize(items: &[UsageItem]) -> Usage {
    let mut by_product: BTreeMap<Option<&str>, Vec<&UsageItem>> = BTreeMap::new();
    let mut by_model: BTreeMap<(Option<&str>, &str), f64> = BTreeMap::new();
    for item in items {
        let key = product_key(item);
        by_product.entry(key).or_default().push(item);
        if let (Some(model), Some(quantity)) = (item.model.as_deref(), item.gross_quantity) {
            if !model.trim().is_empty() {
                *by_model.entry((key, model)).or_insert(0.0) += quantity;
            }
        }
    }

    // The largest models across the whole report, each under its own product:
    // the same model under two products is two entries. `by_model` iterates by
    // (product, model) and the sort is stable, so ties stay by name.
    let mut ranked: Vec<((Option<&str>, &str), f64)> = by_model.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.truncate(TOP_MODELS);

    let mut products: Vec<ProductUse> = by_product
        .into_iter()
        .map(|(key, group)| ProductUse {
            product: key.map(str::to_owned),
            quantity: group.iter().map(|i| i.gross_quantity).sum::<Option<f64>>(),
            models: ranked
                .iter()
                .filter(|((product, _), _)| *product == key)
                .map(|((_, model), quantity)| ModelUse {
                    model: (*model).to_owned(),
                    quantity: *quantity,
                })
                .collect(),
        })
        .collect();
    // `by_product` iterates by name, and the sort is stable, so ties stay by
    // name. The unnamed product is last whatever it holds; among the named,
    // measured come before unmeasured, then the larger first.
    products.sort_by(|a, b| {
        a.product
            .is_none()
            .cmp(&b.product.is_none())
            .then_with(|| match (a.quantity, b.quantity) {
                (Some(x), Some(y)) => y.total_cmp(&x),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            })
    });

    Usage {
        used: sum(items, |i| i.gross_quantity),
        included: sum(items, |i| i.discount_quantity),
        billed_usd: sum(items, |i| i.net_amount),
        products,
    }
}
