use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Hook input data from Claude Code
#[derive(Debug, Deserialize)]
pub struct HookData {
    pub session_id: String,
    pub transcript_path: String,
    pub model: ModelInfo,
    #[serde(default)]
    pub workspace: Option<Workspace>,
    #[serde(default)]
    pub context_window: Option<ContextWindowData>,
    #[serde(default)]
    pub rate_limits: Option<RateLimits>,
}

#[derive(Debug, Deserialize)]
pub struct ModelInfo {
    #[serde(default)]
    pub id: Option<String>,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct Workspace {
    pub current_dir: String,
}

#[derive(Debug, Deserialize)]
pub struct ContextWindowData {
    #[serde(default)]
    pub total_input_tokens: Option<u64>,
    /// Raw model window. Claude Code never exposes the smaller managed window that
    /// auto-compact actually fires against, so this is only the starting point.
    #[serde(default)]
    pub context_window_size: Option<u64>,
    #[serde(default)]
    pub current_usage: Option<UsageTokens>,
}

/// Rate limits from Claude Code statusline stdin (epoch seconds)
#[derive(Debug, Deserialize)]
pub struct RateLimits {
    #[serde(default)]
    pub five_hour: Option<RateLimitWindow>,
    #[serde(default)]
    pub seven_day: Option<RateLimitWindow>,
}

#[derive(Debug, Deserialize)]
pub struct RateLimitWindow {
    pub used_percentage: f64,
    pub resets_at: i64,
}

/// Usage data entry from JSONL
#[derive(Debug, Deserialize)]
pub struct UsageData {
    pub timestamp: String,
    pub message: MessageData,
    #[serde(default, rename = "requestId")]
    pub request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MessageData {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    pub usage: UsageTokens,
}

/// Model id Claude Code writes on assistant messages it generated locally.
const SYNTHETIC_MODEL: &str = "<synthetic>";

impl MessageData {
    /// No API call backs a synthetic message: it is neither billed nor resident in the
    /// context window, and its all-zero usage would read as a real measurement.
    pub fn is_synthetic(&self) -> bool {
        self.model
            .as_deref()
            == Some(SYNTHETIC_MODEL)
    }
}

/// Per-TTL split of `cache_creation_input_tokens`. Long-TTL writes cost more, and the
/// flat total cannot distinguish them.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CacheCreationBreakdown {
    #[serde(default)]
    pub ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    pub ephemeral_1h_input_tokens: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct UsageTokens {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation: Option<CacheCreationBreakdown>,
}

impl UsageTokens {
    /// Total context tokens (input + cache writes + cache reads; excludes output)
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }
}

/// LiteLLM Model Pricing (matching TypeScript schema)
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
    pub const THRESHOLD: u64 = 200_000;

    /// Long-TTL cache writes cost this multiple of the base input price. Used to derive
    /// the rate when LiteLLM omits it, and to bound the published value.
    const CACHE_WRITE_1H_MULTIPLIER: f64 = 2.0;

    pub fn from_prices(base: TokenPrices, tiered: TokenPrices) -> Self {
        Self {
            input_cost_per_token: Some(base.input),
            output_cost_per_token: Some(base.output),
            cache_creation_input_token_cost: Some(base.cache_write),
            cache_creation_input_token_cost_above_1hr: Some(base.cache_write_1h),
            cache_read_input_token_cost: Some(base.cache_read),
            input_cost_per_token_above_200k_tokens: Some(tiered.input),
            output_cost_per_token_above_200k_tokens: Some(tiered.output),
            cache_creation_input_token_cost_above_200k_tokens: Some(tiered.cache_write),
            cache_read_input_token_cost_above_200k_tokens: Some(tiered.cache_read),
        }
    }

    /// Long-TTL write rate. Retired models publish values unrelated to their own base
    /// input price, so an implausible one is replaced by the documented multiple.
    fn cache_write_1h(&self, base_input: f64, base_cache_write: f64) -> f64 {
        let derived = base_input * Self::CACHE_WRITE_1H_MULTIPLIER;
        match self.cache_creation_input_token_cost_above_1hr {
            // Must cost at least a short-TTL write and no more than the derived rate.
            Some(rate) if rate >= base_cache_write && rate <= derived => rate,
            _ => derived,
        }
    }

    /// Total cost for a usage entry.
    ///
    /// The above-threshold tier is selected once from the request's prompt size and then
    /// applies to every category, output included — it is a property of the request, not
    /// of each category's own token count.
    pub fn calculate_cost(&self, usage: &UsageTokens) -> f64 {
        let base_input = self
            .input_cost_per_token
            .unwrap_or(0.0);
        let base_cache_write = self
            .cache_creation_input_token_cost
            .unwrap_or(0.0);

        let prompt_tokens =
            usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens;

        let (input, output, cache_write, cache_read) = if prompt_tokens > Self::THRESHOLD {
            (
                self.input_cost_per_token_above_200k_tokens
                    .unwrap_or(base_input),
                self.output_cost_per_token_above_200k_tokens
                    .unwrap_or_else(|| {
                        self.output_cost_per_token
                            .unwrap_or(0.0)
                    }),
                self.cache_creation_input_token_cost_above_200k_tokens
                    .unwrap_or(base_cache_write),
                self.cache_read_input_token_cost_above_200k_tokens
                    .unwrap_or_else(|| {
                        self.cache_read_input_token_cost
                            .unwrap_or(0.0)
                    }),
            )
        } else {
            (
                base_input,
                self.output_cost_per_token
                    .unwrap_or(0.0),
                base_cache_write,
                self.cache_read_input_token_cost
                    .unwrap_or(0.0),
            )
        };

        // Scale the long-TTL rate with the selected tier so it tracks the premium too.
        let write_1h = self.cache_write_1h(base_input, base_cache_write)
            * if base_cache_write > 0.0 {
                cache_write / base_cache_write
            } else {
                1.0
            };

        let cache_write_cost = match &usage.cache_creation {
            Some(b) if b.ephemeral_5m_input_tokens + b.ephemeral_1h_input_tokens > 0 => {
                b.ephemeral_5m_input_tokens as f64 * cache_write
                    + b.ephemeral_1h_input_tokens as f64 * write_1h
            }
            // No breakdown: the TTL is genuinely unknown, so charge the short-TTL rate.
            _ => usage.cache_creation_input_tokens as f64 * cache_write,
        };

        usage.input_tokens as f64 * input
            + usage.output_tokens as f64 * output
            + cache_write_cost
            + usage.cache_read_input_tokens as f64 * cache_read
    }
}

/// Active 5-hour billing block (only present when a block is actually active)
#[derive(Debug, Clone)]
pub struct ActiveBlock {
    pub start_time: DateTime<Utc>,
    pub cost_usd: f64,
    pub hours_remaining: f64,
}

/// Which limit is critical
#[derive(Debug, PartialEq)]
pub enum LimitType {
    FiveHour,
    SevenDay,
    None,
}

impl LimitType {
    pub fn label(&self) -> &'static str {
        match self {
            LimitType::FiveHour => " 5h",
            LimitType::SevenDay => " 7d",
            LimitType::None => "",
        }
    }
}

/// User's plan type
#[derive(Debug, Clone, Copy)]
pub enum PlanType {
    Api,
    Subscription,
}

/// Burn rate information
#[derive(Debug)]
pub struct BurnRate {
    pub cost_per_hour: f64,
    pub ratio: f64,
    pub seven_day_ratio: f64,
    pub critical_limit: LimitType,
    pub is_at_limit: bool,
    pub reset_in: Option<chrono::Duration>,
    pub seven_day_reset_in: Option<chrono::Duration>,
}

impl Default for BurnRate {
    fn default() -> Self {
        Self {
            cost_per_hour: 0.0,
            ratio: 0.0,
            seven_day_ratio: 0.0,
            critical_limit: LimitType::None,
            is_at_limit: false,
            reset_in: None,
            seven_day_reset_in: None,
        }
    }
}

/// Context information
#[derive(Debug)]
pub struct ContextInfo {
    pub tokens: u64,
    pub percentage: u32,
}

/// Per-window usage data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    pub percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

/// Weekly window scoped to one model bucket, labelled by the server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedUsageWindow {
    pub display_name: String,
    pub percent: f64,
}

/// API usage data from Anthropic API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiUsageData {
    pub five_hour: Option<UsageWindow>,
    pub seven_day: Option<UsageWindow>,
    pub model_scoped: Vec<ScopedUsageWindow>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
