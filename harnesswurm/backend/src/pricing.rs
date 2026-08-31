use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ModelPricing {
    pub name: String,
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

    fn lookup(&self, model: &str, provider: &str) -> Option<&ModelPricing> {
        if let Some(p) = self.by_name.get(model) {
            return Some(p);
        }
        // Real model ids often carry date/version suffixes (e.g.
        // "gpt-4o-2024-08-06", "claude-sonnet-4-5-20250929") that won't
        // exactly match a pricing.yaml entry, so fall back to a prefix match.
        // Multiple entries can match (e.g. both "gpt-4o" and "gpt-4o-mini"
        // prefix-match "gpt-4o-mini-2024-07-18"); HashMap iteration order is
        // randomized, so picking the first match would price the same model
        // differently across runs. Pick the longest — i.e. most specific —
        // matching name deterministically, breaking a same-length tie by
        // preferring the entry whose provider matches this call's.
        self.by_name.values()
            .filter(|p| model.starts_with(p.name.as_str()) || p.name.starts_with(model))
            .max_by_key(|p| (p.name.len(), p.provider == provider))
    }

    /// Returns None when the model isn't in the pricing table, rather than a
    /// fabricated number — an unpriced model should show as unknown, not $0.
    pub fn estimate_cost(
        &self,
        model: &str,
        provider: &str,
        regular_input_tokens: i64,
        cache_write_tokens: i64,
        cache_read_tokens: i64,
        output_tokens: i64,
    ) -> Option<f64> {
        let pricing = self.lookup(model, provider)?;
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

    fn table_with(entries: &[(&str, &str, f64, f64, f64)]) -> PricingTable {
        let by_name = entries.iter().map(|&(name, provider, input, cache_read, output)| {
            (name.to_string(), ModelPricing {
                name: name.to_string(),
                provider: provider.to_string(),
                input_per_million: Some(input),
                cache_write_per_million: None,
                cache_read_per_million: Some(cache_read),
                output_per_million: Some(output),
            })
        }).collect();
        PricingTable { by_name }
    }

    fn sample_table() -> PricingTable {
        table_with(&[("gpt-4o", "openai", 2.5, 1.25, 10.0)])
    }

    #[test]
    fn unknown_model_returns_none() {
        let table = sample_table();
        assert_eq!(table.estimate_cost("some-unlisted-model", "openai", 100, 0, 0, 100), None);
    }

    #[test]
    fn prefix_match_handles_dated_model_ids() {
        let table = sample_table();
        assert!(table.estimate_cost("gpt-4o-2024-08-06", "openai", 1_000_000, 0, 0, 0).is_some());
    }

    #[test]
    fn cost_uses_cache_read_discount() {
        let table = sample_table();
        let cost = table.estimate_cost("gpt-4o", "openai", 0, 0, 1_000_000, 0).unwrap();
        assert!((cost - 1.25).abs() < 1e-9);
    }

    #[test]
    fn prefix_match_prefers_the_longer_more_specific_name() {
        // Both "gpt-4o" and "gpt-4o-mini" prefix-match this dated id; picking
        // "gpt-4o" here would price a mini request at ~17x its real cost.
        let table = table_with(&[
            ("gpt-4o", "openai", 2.5, 1.25, 10.0),
            ("gpt-4o-mini", "openai", 0.15, 0.075, 0.60),
        ]);
        let cost = table.estimate_cost("gpt-4o-mini-2024-07-18", "openai", 1_000_000, 0, 0, 0).unwrap();
        assert!((cost - 0.15).abs() < 1e-9);
    }

    #[test]
    fn same_length_tie_prefers_matching_provider() {
        // Neither catalog name is a prefix of the other, but both are the
        // same length and both match "mini" via the reverse (catalog name
        // is more specific than the queried alias) direction of the prefix
        // check, so length alone can't break the tie.
        let table = table_with(&[
            ("mini-2024", "openai", 1.0, 0.5, 2.0),
            ("mini-2025", "anthropic", 9.0, 4.5, 18.0),
        ]);
        let cost = table.estimate_cost("mini", "anthropic", 1_000_000, 0, 0, 0).unwrap();
        assert!((cost - 9.0).abs() < 1e-9);
    }
}
