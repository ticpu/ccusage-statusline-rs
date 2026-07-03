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
    pub used_percentage: Option<f64>,
    #[serde(default)]
    pub total_input_tokens: Option<u64>,
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

#[derive(Debug, Deserialize)]
pub struct UsageTokens {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
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

/// Per-token prices for the four token categories
#[derive(Clone, Copy)]
pub struct TokenPrices {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

impl ModelPricing {
    pub const THRESHOLD: u64 = 200_000;

    pub fn from_prices(base: TokenPrices, tiered: TokenPrices) -> Self {
        Self {
            input_cost_per_token: Some(base.input),
            output_cost_per_token: Some(base.output),
            cache_creation_input_token_cost: Some(base.cache_write),
            cache_read_input_token_cost: Some(base.cache_read),
            input_cost_per_token_above_200k_tokens: Some(tiered.input),
            output_cost_per_token_above_200k_tokens: Some(tiered.output),
            cache_creation_input_token_cost_above_200k_tokens: Some(tiered.cache_write),
            cache_read_input_token_cost_above_200k_tokens: Some(tiered.cache_read),
        }
    }

    /// Calculate total cost for a usage entry using tiered pricing.
    /// Converts to (base, tiered) pair once, then iterates over the four token categories.
    pub fn calculate_cost(&self, usage: &UsageTokens) -> f64 {
        let base = TokenPrices {
            input: self
                .input_cost_per_token
                .unwrap_or(0.0),
            output: self
                .output_cost_per_token
                .unwrap_or(0.0),
            cache_write: self
                .cache_creation_input_token_cost
                .unwrap_or(0.0),
            cache_read: self
                .cache_read_input_token_cost
                .unwrap_or(0.0),
        };
        let tiered = TokenPrices {
            input: self
                .input_cost_per_token_above_200k_tokens
                .unwrap_or(base.input),
            output: self
                .output_cost_per_token_above_200k_tokens
                .unwrap_or(base.output),
            cache_write: self
                .cache_creation_input_token_cost_above_200k_tokens
                .unwrap_or(base.cache_write),
            cache_read: self
                .cache_read_input_token_cost_above_200k_tokens
                .unwrap_or(base.cache_read),
        };

        [
            (usage.input_tokens, base.input, tiered.input),
            (usage.output_tokens, base.output, tiered.output),
            (
                usage.cache_creation_input_tokens,
                base.cache_write,
                tiered.cache_write,
            ),
            (
                usage.cache_read_input_tokens,
                base.cache_read,
                tiered.cache_read,
            ),
        ]
        .iter()
        .map(|&(tokens, base_p, tiered_p)| {
            if tokens == 0 {
                return 0.0;
            }
            if tokens <= Self::THRESHOLD {
                tokens as f64 * base_p
            } else {
                Self::THRESHOLD as f64 * base_p + (tokens - Self::THRESHOLD) as f64 * tiered_p
            }
        })
        .sum()
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

/// API usage data from Anthropic API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiUsageData {
    pub five_hour: Option<UsageWindow>,
    pub seven_day: Option<UsageWindow>,
    pub seven_day_sonnet: Option<f64>,
}
