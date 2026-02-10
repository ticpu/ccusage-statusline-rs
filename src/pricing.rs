use crate::types::{ModelPricing, PricingCache, TokenPrices, UsageData};
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

    let pricing = if model.starts_with("claude-opus") {
        // Opus family: $15/M input, $75/M output, no tiered pricing
        let prices = TokenPrices {
            input: 15e-6,
            output: 75e-6,
            cache_write: 18.75e-6,
            cache_read: 1.5e-6,
        };
        ModelPricing::from_prices(prices, prices)
    } else if model.starts_with("claude-sonnet-4-5") {
        // Sonnet 4.5: same base as Sonnet 4, no tiered pricing
        let prices = TokenPrices {
            input: 3e-6,
            output: 15e-6,
            cache_write: 3.75e-6,
            cache_read: 3e-7,
        };
        ModelPricing::from_prices(prices, prices)
    } else {
        // Default: Sonnet 4 with tiered pricing above 200k
        let base = TokenPrices {
            input: 3e-6,
            output: 15e-6,
            cache_write: 3.75e-6,
            cache_read: 3e-7,
        };
        let tiered = TokenPrices {
            input: 6e-6,
            output: 22.5e-6,
            cache_write: 7.5e-6,
            cache_read: 6e-7,
        };
        ModelPricing::from_prices(base, tiered)
    };

    pricing.calculate_cost(&entry.message.usage)
}
