//! Wire shape of the usage reads. Field names are GitHub's.

use serde::Deserialize;

/// `{ "usageItems": [ … ] }`.
#[derive(Debug, Deserialize)]
pub(crate) struct UsageReport {
    #[serde(rename = "usageItems")]
    pub(crate) usage_items: Vec<UsageItem>,
}

/// One line of a usage report. Every figure is an `Option`: an absent key is
/// unmeasured, never zero.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UsageItem {
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub sku: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(rename = "unitType", default)]
    pub unit_type: Option<String>,
    #[serde(rename = "pricePerUnit", default)]
    pub price_per_unit: Option<f64>,
    #[serde(rename = "grossQuantity", default)]
    pub gross_quantity: Option<f64>,
    #[serde(rename = "grossAmount", default)]
    pub gross_amount: Option<f64>,
    /// The included part: credits the plan absorbed.
    #[serde(rename = "discountQuantity", default)]
    pub discount_quantity: Option<f64>,
    #[serde(rename = "discountAmount", default)]
    pub discount_amount: Option<f64>,
    #[serde(rename = "netQuantity", default)]
    pub net_quantity: Option<f64>,
    /// What is billed.
    #[serde(rename = "netAmount", default)]
    pub net_amount: Option<f64>,
}
