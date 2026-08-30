use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ModelPricing {
    pub name: String,
    #[allow(dead_code)]
    pub provider: String,
    #[serde(default)]
    pub input_per_million: Option<f64>,
    #[serde(default)]
    pub cache_write_per_million: Option<f64>,
    #[serde(default)]
    pub cache_read_per_million: Option<f64>,
    #[serde(default)]
    pub output_per_million: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct PricingFile {
    models: Vec<ModelPricing>,
}

#[derive(Debug, Clone, Default)]
pub struct PricingTable {
    by_name: HashMap<String, ModelPricing>,
}

impl PricingTable {
    pub fn load(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(file) = serde_yaml::from_str::<PricingFile>(&content) else {
            eprintln!("Warning: failed to parse {}, cost estimates disabled", path.display());
            return Self::default();
        };
        let by_name = file.models.into_iter().map(|m| (m.name.clone(), m)).collect();
        Self { by_name }
    }

    fn lookup(&self, model: &str) -> Option<&ModelPricing> {
        if let Some(p) = self.by_name.get(model) {
            return Some(p);
        }
        // Real model ids often carry date/version suffixes (e.g.
        // "gpt-4o-2024-08-06", "claude-sonnet-4-5-20250929") that won't
        // exactly match a pricing.yaml entry, so fall back to a prefix match.
        self.by_name.values().find(|p| model.starts_with(p.name.as_str()) || p.name.starts_with(model))
    }

    /// Returns None when the model isn't in the pricing table, rather than a
    /// fabricated number — an unpriced model should show as unknown, not $0.
    pub fn estimate_cost(
        &self,
        model: &str,
        regular_input_tokens: i64,
        cache_write_tokens: i64,
        cache_read_tokens: i64,
        output_tokens: i64,
    ) -> Option<f64> {
        let pricing = self.lookup(model)?;
        let input_price = pricing.input_per_million?;
        let output_price = pricing.output_per_million?;
        let cache_write_price = pricing.cache_write_per_million.unwrap_or(input_price);
        let cache_read_price = pricing.cache_read_per_million.unwrap_or(input_price);

        let cost = (regular_input_tokens as f64 * input_price
            + cache_write_tokens as f64 * cache_write_price
            + cache_read_tokens as f64 * cache_read_price
            + output_tokens as f64 * output_price)
            / 1_000_000.0;
        Some(cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_table() -> PricingTable {
        let by_name = HashMap::from([(
            "gpt-4o".to_string(),
            ModelPricing {
                name: "gpt-4o".to_string(),
                provider: "openai".to_string(),
                input_per_million: Some(2.5),
                cache_write_per_million: None,
                cache_read_per_million: Some(1.25),
                output_per_million: Some(10.0),
            },
        )]);
        PricingTable { by_name }
    }

    #[test]
    fn unknown_model_returns_none() {
        let table = sample_table();
        assert_eq!(table.estimate_cost("some-unlisted-model", 100, 0, 0, 100), None);
    }

    #[test]
    fn prefix_match_handles_dated_model_ids() {
        let table = sample_table();
        assert!(table.estimate_cost("gpt-4o-2024-08-06", 1_000_000, 0, 0, 0).is_some());
    }

    #[test]
    fn cost_uses_cache_read_discount() {
        let table = sample_table();
        let cost = table.estimate_cost("gpt-4o", 0, 0, 1_000_000, 0).unwrap();
        assert!((cost - 1.25).abs() < 1e-9);
    }
}
