use crate::types::{ModelPricing, TokenPrices, UsageTokens};
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

/// The upstream table is a couple of megabytes and grows with every model added.
const MAX_PRICING_BYTES: u64 = 64 * 1024 * 1024;

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
                let body = crate::http::read_body_limited(response, MAX_PRICING_BYTES)?;
                let cache = PricingCache {
                    timestamp: Utc::now().timestamp(),
                    models: serde_json::from_slice(&body)
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

    /// Cost from model id and token counts alone, so cached entries need not
    /// reconstruct a whole transcript record to be re-priced.
    pub fn calculate_cost_for(&self, model: Option<&str>, usage: &UsageTokens) -> f64 {
        if let Some(name) = model
            && let Some(pricing) = self.get_model_pricing(name)
        {
            return pricing.calculate_cost(usage);
        }
        estimate_cost_fallback(model, usage)
    }
}

/// Cache rates are fixed multiples of the base input price, so a family only needs its
/// two published per-token rates.
const CACHE_WRITE_5M_MULTIPLIER: f64 = 1.25;
const CACHE_WRITE_1H_MULTIPLIER: f64 = 2.0;
const CACHE_READ_MULTIPLIER: f64 = 0.1;

fn prices_from(input: f64, output: f64) -> TokenPrices {
    TokenPrices {
        input,
        output,
        cache_write: input * CACHE_WRITE_5M_MULTIPLIER,
        cache_write_1h: input * CACHE_WRITE_1H_MULTIPLIER,
        cache_read: input * CACHE_READ_MULTIPLIER,
    }
}

/// Fallback cost estimation for a model LiteLLM does not list. Current models carry no
/// above-threshold tier, so base and tiered prices are the same.
fn estimate_cost_fallback(model: Option<&str>, usage: &UsageTokens) -> f64 {
    let prices = match model {
        Some(m) if m.contains("opus") => prices_from(5e-6, 25e-6),
        Some(m) if m.contains("haiku") => prices_from(1e-6, 5e-6),
        Some(m) if m.contains("sonnet") => prices_from(3e-6, 15e-6),
        other => {
            if std::io::stderr().is_terminal() {
                eprintln!(
                    "pricing: {} not in LiteLLM and not a known family, estimating at Sonnet rates",
                    other.unwrap_or("(no model id)")
                );
            }
            prices_from(3e-6, 15e-6)
        }
    };

    ModelPricing::from_prices(prices, prices).calculate_cost(usage)
}
