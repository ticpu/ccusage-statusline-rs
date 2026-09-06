use crate::types::UsageTokens;
use crate::warn;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Cached pricing data with timestamp
#[derive(Debug, Deserialize, Serialize)]
struct PricingCache {
    timestamp: i64,
    models: HashMap<String, ModelPricing>,
}

/// The upstream table is a couple of megabytes and grows with every model added.
const MAX_PRICING_BYTES: u64 = 64 * 1024 * 1024;

/// One model's entry in the LiteLLM table.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ModelPricing {
    #[serde(default)]
    pub input_cost_per_token: Option<f64>,
    #[serde(default)]
    pub output_cost_per_token: Option<f64>,
    #[serde(default)]
    pub cache_creation_input_token_cost: Option<f64>,
    #[serde(default)]
    pub cache_creation_input_token_cost_above_1hr: Option<f64>,
    #[serde(default)]
    pub cache_read_input_token_cost: Option<f64>,
    #[serde(default)]
    pub input_cost_per_token_above_200k_tokens: Option<f64>,
    #[serde(default)]
    pub output_cost_per_token_above_200k_tokens: Option<f64>,
    #[serde(default)]
    pub cache_creation_input_token_cost_above_200k_tokens: Option<f64>,
    #[serde(default)]
    pub cache_read_input_token_cost_above_200k_tokens: Option<f64>,
}

/// Per-token prices for the four token categories. `cache_write` is the short-TTL
/// rate; long-TTL writes are priced separately via `cache_write_1h`.
#[derive(Clone, Copy)]
pub struct TokenPrices {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

impl ModelPricing {
    const THRESHOLD: u64 = 200_000;

    /// Long-TTL write rate. Retired models publish values unrelated to their own base
    /// input price, so an implausible one is replaced by the documented multiple.
    fn cache_write_1h(&self, base_input: f64, base_cache_write: f64) -> f64 {
        let derived = base_input * CACHE_WRITE_1H_MULTIPLIER;
        match self.cache_creation_input_token_cost_above_1hr {
            // Must cost at least a short-TTL write and no more than the derived rate.
            Some(rate) if rate >= base_cache_write && rate <= derived => rate,
            _ => derived,
        }
    }

    /// Rates a request of this prompt size is charged at.
    ///
    /// The above-threshold tier is selected once from the request's prompt size and then
    /// applies to every category, output included — it is a property of the request, not
    /// of each category's own token count.
    pub fn tier(&self, prompt_tokens: u64) -> TokenPrices {
        let base_input = self
            .input_cost_per_token
            .unwrap_or(0.0);
        let base_output = self
            .output_cost_per_token
            .unwrap_or(0.0);
        let base_cache_write = self
            .cache_creation_input_token_cost
            .unwrap_or(0.0);
        let base_cache_read = self
            .cache_read_input_token_cost
            .unwrap_or(0.0);

        let (input, output, cache_write, cache_read) = if prompt_tokens > Self::THRESHOLD {
            (
                self.input_cost_per_token_above_200k_tokens
                    .unwrap_or(base_input),
                self.output_cost_per_token_above_200k_tokens
                    .unwrap_or(base_output),
                self.cache_creation_input_token_cost_above_200k_tokens
                    .unwrap_or(base_cache_write),
                self.cache_read_input_token_cost_above_200k_tokens
                    .unwrap_or(base_cache_read),
            )
        } else {
            (base_input, base_output, base_cache_write, base_cache_read)
        };

        // Scale the long-TTL rate with the selected tier so it tracks the premium too.
        let premium = if base_cache_write > 0.0 {
            cache_write / base_cache_write
        } else {
            1.0
        };

        TokenPrices {
            input,
            output,
            cache_write,
            cache_write_1h: self.cache_write_1h(base_input, base_cache_write) * premium,
            cache_read,
        }
    }

    /// Total cost for a usage entry.
    pub fn calculate_cost(&self, usage: &UsageTokens) -> f64 {
        cost_at(&self.tier(usage.context_tokens()), usage)
    }
}

pub(crate) fn cost_at(prices: &TokenPrices, usage: &UsageTokens) -> f64 {
    let cache_write_cost = match &usage.cache_creation {
        Some(b) if b.ephemeral_5m_input_tokens + b.ephemeral_1h_input_tokens > 0 => {
            b.ephemeral_5m_input_tokens as f64 * prices.cache_write
                + b.ephemeral_1h_input_tokens as f64 * prices.cache_write_1h
        }
        // No breakdown: the TTL is genuinely unknown, so charge the short-TTL rate.
        _ => usage.cache_creation_input_tokens as f64 * prices.cache_write,
    };

    usage.input_tokens as f64 * prices.input
        + usage.output_tokens as f64 * prices.output
        + cache_write_cost
        + usage.cache_read_input_tokens as f64 * prices.cache_read
}

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

        let cached: Option<PricingCache> = crate::cache::read_json_warn(&pricing_cache_path);

        // Return a fresh cache by move; otherwise keep it as the stale fallback below.
        let cached = match cached {
            Some(c) if Utc::now().timestamp() - c.timestamp < Self::MAX_AGE_SECONDS => {
                return Ok(c.models);
            }
            other => other,
        };

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
                if let Err(e) = crate::cache::write_json_atomic(&pricing_cache_path, &cache) {
                    warn!("pricing cache write failed: {:#}", e);
                }
                let PricingCache { models, .. } = cache;
                Ok(models)
            }
            Ok(response) => {
                let status = response.status();
                warn!("pricing fetch failed (HTTP {}), using stale cache", status);
                cached
                    .map(|c| c.models)
                    .context("Failed to fetch pricing data and no cache available")
            }
            Err(e) => {
                warn!("pricing fetch error, using stale cache: {:#}", e);
                if let Some(c) = cached {
                    Ok(c.models)
                } else {
                    Err(anyhow::Error::from(e))
                        .context("Failed to fetch pricing and no cache available")
                }
            }
        }
    }

    /// Table entry for a model id, if the table lists it.
    ///
    /// The last resort scans every key, so callers price a whole render's entries
    /// through one resolution per model id rather than one per entry.
    pub(crate) fn resolve(&self, model_name: &str) -> Option<&ModelPricing> {
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
        for (key, pricing) in &self.models {
            if key.eq_ignore_ascii_case(model_name) {
                return Some(pricing);
            }
        }

        None
    }
}

/// Cache rates are fixed multiples of the base input price, so a family only needs its
/// two published per-token rates.
const CACHE_WRITE_5M_MULTIPLIER: f64 = 1.25;
/// Also bounds a published long-TTL rate, since a retired model's is unrelated to its own
/// base input price.
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

/// Estimated rates for a model LiteLLM does not list. Current models carry no
/// above-threshold tier, so one set of prices covers every prompt size.
pub(crate) fn estimate_prices(model: Option<&str>) -> TokenPrices {
    match model {
        Some(m) if m.contains("opus") => prices_from(5e-6, 25e-6),
        Some(m) if m.contains("haiku") => prices_from(1e-6, 5e-6),
        Some(m) if m.contains("sonnet") => prices_from(3e-6, 15e-6),
        other => {
            warn!(
                "pricing: {} not in LiteLLM and not a known family, estimating at Sonnet rates",
                other.unwrap_or("(no model id)")
            );
            prices_from(3e-6, 15e-6)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CacheCreationBreakdown;

    fn sonnet_4_5() -> ModelPricing {
        // LiteLLM values for claude-sonnet-4-5, the tier-bearing model family
        ModelPricing {
            input_cost_per_token: Some(3e-6),
            output_cost_per_token: Some(15e-6),
            cache_creation_input_token_cost: Some(3.75e-6),
            cache_creation_input_token_cost_above_1hr: Some(6e-6),
            cache_read_input_token_cost: Some(3e-7),
            input_cost_per_token_above_200k_tokens: Some(6e-6),
            output_cost_per_token_above_200k_tokens: Some(22.5e-6),
            cache_creation_input_token_cost_above_200k_tokens: Some(7.5e-6),
            cache_read_input_token_cost_above_200k_tokens: Some(6e-7),
        }
    }

    fn usage(input: u64, output: u64, write: u64, read: u64) -> UsageTokens {
        UsageTokens {
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: write,
            cache_read_input_tokens: read,
            cache_creation: None,
        }
    }

    #[test]
    fn test_tier_selected_by_prompt_size_applies_to_output() {
        // Prompt clears the threshold on cache reads alone; output must still price premium.
        let u = usage(150, 2_000, 5_000, 400_000);
        let cost = sonnet_4_5().calculate_cost(&u);
        let expected = 150.0 * 6e-6 + 2_000.0 * 22.5e-6 + 5_000.0 * 7.5e-6 + 400_000.0 * 6e-7;
        assert!(
            (cost - expected).abs() < 1e-9,
            "got {cost}, expected {expected}"
        );
    }

    #[test]
    fn test_below_threshold_uses_base_tier_throughout() {
        let u = usage(1_000, 500, 2_000, 10_000);
        let cost = sonnet_4_5().calculate_cost(&u);
        let expected = 1_000.0 * 3e-6 + 500.0 * 15e-6 + 2_000.0 * 3.75e-6 + 10_000.0 * 3e-7;
        assert!((cost - expected).abs() < 1e-9);
    }

    #[test]
    fn test_no_tier_fields_means_base_rates() {
        // 4.6+ models omit the above-threshold fields; a huge prompt stays at base.
        let mut p = sonnet_4_5();
        p.input_cost_per_token_above_200k_tokens = None;
        p.output_cost_per_token_above_200k_tokens = None;
        p.cache_creation_input_token_cost_above_200k_tokens = None;
        p.cache_read_input_token_cost_above_200k_tokens = None;

        let u = usage(150, 2_000, 5_000, 400_000);
        let expected = 150.0 * 3e-6 + 2_000.0 * 15e-6 + 5_000.0 * 3.75e-6 + 400_000.0 * 3e-7;
        assert!((p.calculate_cost(&u) - expected).abs() < 1e-9);
    }

    #[test]
    fn test_long_ttl_writes_cost_more_than_short() {
        let mut short = usage(0, 0, 10_000, 0);
        short.cache_creation = Some(CacheCreationBreakdown {
            ephemeral_5m_input_tokens: 10_000,
            ephemeral_1h_input_tokens: 0,
        });
        let mut long = usage(0, 0, 10_000, 0);
        long.cache_creation = Some(CacheCreationBreakdown {
            ephemeral_5m_input_tokens: 0,
            ephemeral_1h_input_tokens: 10_000,
        });

        let p = sonnet_4_5();
        assert!((p.calculate_cost(&short) - 10_000.0 * 3.75e-6).abs() < 1e-9);
        assert!((p.calculate_cost(&long) - 10_000.0 * 6e-6).abs() < 1e-9);
    }

    #[test]
    fn test_absent_breakdown_falls_back_to_short_ttl_rate() {
        let u = usage(0, 0, 10_000, 0);
        assert!((sonnet_4_5().calculate_cost(&u) - 10_000.0 * 3.75e-6).abs() < 1e-9);
    }

    /// Above the threshold the long-TTL write rate rises with the tier like every other
    /// category; leaving it at its base value under-charges the largest requests.
    #[test]
    fn test_tiered_long_ttl_write_scales_with_tier() {
        let mut u = usage(0, 0, 10_000, 400_000);
        u.cache_creation = Some(CacheCreationBreakdown {
            ephemeral_5m_input_tokens: 0,
            ephemeral_1h_input_tokens: 10_000,
        });
        let expected = 10_000.0 * 12e-6 + 400_000.0 * 6e-7;
        assert!((sonnet_4_5().calculate_cost(&u) - expected).abs() < 1e-9);
    }

    #[test]
    fn test_implausible_published_1h_rate_is_replaced() {
        // Retired models publish a 1h rate unrelated to their own base input price.
        let mut p = sonnet_4_5();
        p.cache_creation_input_token_cost_above_1hr = Some(6e-6 * 24.0);
        let mut u = usage(0, 0, 1_000, 0);
        u.cache_creation = Some(CacheCreationBreakdown {
            ephemeral_5m_input_tokens: 0,
            ephemeral_1h_input_tokens: 1_000,
        });
        // Falls back to twice base input, not the published nonsense.
        assert!((p.calculate_cost(&u) - 1_000.0 * 6e-6).abs() < 1e-9);
    }
}
