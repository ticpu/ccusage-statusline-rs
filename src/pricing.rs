use crate::types::{ModelPricing, PricingCache, UsageData};
use anyhow::{Context, Result};
use chrono::Utc;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Pricing fetcher with caching
pub struct PricingFetcher {
    models: HashMap<String, ModelPricing>,
}

impl PricingFetcher {
    const LITELLM_URL: &'static str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
    const MAX_AGE_SECONDS: i64 = 86400; // 24 hours

    /// Create a new pricing fetcher and load pricing data
    pub fn new(cache_dir: &Path) -> Result<Self> {
        let models = Self::load_pricing(cache_dir)?;
        Ok(Self { models })
    }

    /// Load pricing from cache or fetch from LiteLLM
    fn load_pricing(cache_dir: &Path) -> Result<HashMap<String, ModelPricing>> {
        let pricing_cache_path = cache_dir.join("pricing.json");

        // Check if cache exists and is fresh
        if let Ok(cache_file) = fs::read_to_string(&pricing_cache_path)
            && let Ok(cached) = serde_json::from_str::<PricingCache>(&cache_file)
        {
            let now = Utc::now().timestamp();
            let age = now - cached.timestamp;

            if age < Self::MAX_AGE_SECONDS {
                return Ok(cached.models);
            }
        }

        // Try to fetch fresh pricing
        match reqwest::blocking::get(Self::LITELLM_URL) {
            Ok(response) if response.status().is_success() => {
                let models: HashMap<String, ModelPricing> =
                    response.json().context("Failed to parse pricing JSON")?;

                // Cache the result
                let cache = PricingCache {
                    timestamp: Utc::now().timestamp(),
                    models: models.clone(),
                };

                if let Ok(cache_json) = serde_json::to_string_pretty(&cache) {
                    let _ = fs::write(&pricing_cache_path, cache_json);
                }

                Ok(models)
            }
            _ => {
                // Network error or bad response, try to use stale cache
                if let Ok(cache_file) = fs::read_to_string(&pricing_cache_path)
                    && let Ok(cached) = serde_json::from_str::<PricingCache>(&cache_file)
                {
                    return Ok(cached.models);
                }
                anyhow::bail!("Failed to fetch pricing and no cache available")
            }
        }
    }

    /// Get pricing for a specific model
    fn get_model_pricing(&self, model_name: &str) -> Option<&ModelPricing> {
        // Try exact match first
        if let Some(pricing) = self.models.get(model_name) {
            return Some(pricing);
        }

        // Try with common prefixes
        let prefixes = ["anthropic/", "claude-", "openai/"];
        for prefix in &prefixes {
            let candidate = format!("{}{}", prefix, model_name);
            if let Some(pricing) = self.models.get(&candidate) {
                return Some(pricing);
            }
        }

        // Try case-insensitive match
        let model_lower = model_name.to_lowercase();
        for (key, pricing) in &self.models {
            if key.to_lowercase() == model_lower {
                return Some(pricing);
            }
        }

        None
    }

    /// Calculate cost for a usage entry
    pub fn calculate_entry_cost(&self, entry: &UsageData) -> f64 {
        if let Some(model_name) = &entry.message.model
            && let Some(pricing) = self.get_model_pricing(model_name)
        {
            return pricing.calculate_cost(&entry.message.usage);
        }
        // Fallback to hardcoded estimate if model not found
        estimate_cost_fallback(entry)
    }
}

/// Fallback cost estimation with hardcoded prices (when model not found in LiteLLM)
fn estimate_cost_fallback(entry: &UsageData) -> f64 {
    let model = entry
        .message
        .model
        .as_deref()
        .unwrap_or("claude-sonnet-4-20250514");

    // LiteLLM pricing (per token, not per million)
    let (
        input_price,
        output_price,
        cache_write_price,
        cache_read_price,
        input_price_above_200k,
        output_price_above_200k,
        cache_write_price_above_200k,
        cache_read_price_above_200k,
    ) = match model {
        "claude-sonnet-4-20250514" => (
            3e-6,    // input
            15e-6,   // output
            3.75e-6, // cache_creation
            3e-7,    // cache_read
            6e-6,    // input_above_200k
            22.5e-6, // output_above_200k
            7.5e-6,  // cache_creation_above_200k
            6e-7,    // cache_read_above_200k
        ),
        "claude-sonnet-4-5-20250929" => (
            3e-6,    // input (no tiered pricing)
            15e-6,   // output
            3.75e-6, // cache_creation
            3e-7,    // cache_read
            3e-6,    // same as base (no tiered pricing)
            15e-6, 3.75e-6, 3e-7,
        ),
        "claude-opus-4-1-20250805" => (
            15e-6,    // input (5x more expensive)
            75e-6,    // output
            18.75e-6, // cache_creation
            1.5e-6,   // cache_read (5x more expensive)
            15e-6,    // no tiered pricing
            75e-6, 18.75e-6, 1.5e-6,
        ),
        _ => (3e-6, 15e-6, 3.75e-6, 3e-7, 6e-6, 22.5e-6, 7.5e-6, 6e-7), // default to Sonnet 4
    };

    // Helper for tiered cost calculation (200k threshold for Claude models)
    let calc_tiered = |tokens: u64, base_price: f64, tiered_price: f64| -> f64 {
        if tokens <= 200_000 {
            tokens as f64 * base_price
        } else {
            (200_000.0 * base_price) + ((tokens - 200_000) as f64 * tiered_price)
        }
    };

    let input_cost = calc_tiered(
        entry.message.usage.input_tokens,
        input_price,
        input_price_above_200k,
    );
    let output_cost = calc_tiered(
        entry.message.usage.output_tokens,
        output_price,
        output_price_above_200k,
    );
    let cache_write_cost = calc_tiered(
        entry.message.usage.cache_creation_input_tokens,
        cache_write_price,
        cache_write_price_above_200k,
    );
    let cache_read_cost = calc_tiered(
        entry.message.usage.cache_read_input_tokens,
        cache_read_price,
        cache_read_price_above_200k,
    );

    input_cost + output_cost + cache_write_cost + cache_read_cost
}