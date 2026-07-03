use crate::types::{ModelPricing, TokenPrices, UsageData};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Cached pricing data with timestamp
#[derive(Debug, Deserialize, Serialize)]
struct PricingCache {
    timestamp: i64,
    models: HashMap<String, ModelPricing>,
}
use std::io::IsTerminal;
use std::path::Path;

/// Pricing fetcher with caching
pub struct PricingFetcher {
    models: HashMap<String, ModelPricing>,
}

impl PricingFetcher {
    const LITELLM_URL: &'static str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
    const MAX_AGE_SECONDS: i64 = 86400;

    /// Create a new pricing fetcher and load pricing data
    pub fn new(cache_dir: &Path) -> Result<Self> {
        let models = Self::load_pricing(cache_dir)?;
        Ok(Self { models })
    }

    /// Load pricing from cache or fetch from LiteLLM
    fn load_pricing(cache_dir: &Path) -> Result<HashMap<String, ModelPricing>> {
        let pricing_cache_path = cache_dir.join("pricing.json");

        let cached: Option<PricingCache> = match crate::cache::read_json(&pricing_cache_path) {
            Ok(v) => v,
            Err(e) => {
                if std::io::stderr().is_terminal() {
                    eprintln!("pricing cache read error: {:#}", e);
                }
                None
            }
        };

        // Determine freshness without consuming `cached`
        let is_fresh = cached
            .as_ref()
            .map(|c| Utc::now().timestamp() - c.timestamp < Self::MAX_AGE_SECONDS)
            .unwrap_or(false);

        if is_fresh {
            // Move out without clone — cached is consumed here
            return Ok(cached
                .unwrap()
                .models);
        }

        match crate::http::http_client()?
            .get(Self::LITELLM_URL)
            .send()
        {
            Ok(response)
                if response
                    .status()
                    .is_success() =>
            {
                let cache = PricingCache {
                    timestamp: Utc::now().timestamp(),
                    models: response
                        .json()
                        .context("Failed to parse pricing JSON")?,
                };
                if let Err(e) = crate::cache::write_json_atomic(&pricing_cache_path, &cache)
                    && std::io::stderr().is_terminal()
                {
                    eprintln!("pricing cache write failed: {:#}", e);
                }
                let PricingCache { models, .. } = cache;
                Ok(models)
            }
            Ok(response) => {
                let status = response.status();
                if std::io::stderr().is_terminal() {
                    eprintln!("pricing fetch failed (HTTP {}), using stale cache", status);
                }
                cached
                    .map(|c| c.models)
                    .context("Failed to fetch pricing data and no cache available")
            }
            Err(e) => {
                if std::io::stderr().is_terminal() {
                    eprintln!("pricing fetch error, using stale cache: {:#}", e);
                }
                if let Some(c) = cached {
                    Ok(c.models)
                } else {
                    Err(anyhow::Error::from(e))
                        .context("Failed to fetch pricing and no cache available")
                }
            }
        }
    }

    /// Get pricing for a specific model
    fn get_model_pricing(&self, model_name: &str) -> Option<&ModelPricing> {
        // Try exact match first
        if let Some(pricing) = self
            .models
            .get(model_name)
        {
            return Some(pricing);
        }

        // Try with common prefixes
        let prefixes = ["anthropic/", "claude-", "openai/"];
        for prefix in &prefixes {
            let candidate = format!("{}{}", prefix, model_name);
            if let Some(pricing) = self
                .models
                .get(&candidate)
            {
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
        if let Some(model_name) = &entry
            .message
            .model
            && let Some(pricing) = self.get_model_pricing(model_name)
        {
            return pricing.calculate_cost(
                &entry
                    .message
                    .usage,
            );
        }
        // Fallback to hardcoded estimate if model not found in LiteLLM
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
        let prices = TokenPrices {
            input: 15e-6,
            output: 75e-6,
            cache_write: 18.75e-6,
            cache_read: 1.5e-6,
        };
        ModelPricing::from_prices(prices, prices)
    } else if model.starts_with("claude-sonnet-4-5") {
        let prices = TokenPrices {
            input: 3e-6,
            output: 15e-6,
            cache_write: 3.75e-6,
            cache_read: 3e-7,
        };
        ModelPricing::from_prices(prices, prices)
    } else {
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

    pricing.calculate_cost(
        &entry
            .message
            .usage,
    )
}
