use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub current_usage: Option<ContextUsage>,
}

#[derive(Debug, Deserialize)]
pub struct ContextUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
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
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
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

    /// Calculate cost with tiered pricing
    pub fn calculate_tiered_cost(
        &self,
        tokens: u64,
        base_price: Option<f64>,
        tiered_price: Option<f64>,
    ) -> f64 {
        if tokens == 0 {
            return 0.0;
        }

        let base = base_price.unwrap_or(0.0);

        if tokens <= Self::THRESHOLD {
            tokens as f64 * base
        } else {
            let tiered = tiered_price.unwrap_or(base);
            (Self::THRESHOLD as f64 * base) + ((tokens - Self::THRESHOLD) as f64 * tiered)
        }
    }

    /// Calculate total cost for a usage entry
    pub fn calculate_cost(&self, usage: &UsageTokens) -> f64 {
        let input_cost = self.calculate_tiered_cost(
            usage.input_tokens,
            self.input_cost_per_token,
            self.input_cost_per_token_above_200k_tokens,
        );

        let output_cost = self.calculate_tiered_cost(
            usage.output_tokens,
            self.output_cost_per_token,
            self.output_cost_per_token_above_200k_tokens,
        );

        let cache_write_cost = self.calculate_tiered_cost(
            usage.cache_creation_input_tokens,
            self.cache_creation_input_token_cost,
            self.cache_creation_input_token_cost_above_200k_tokens,
        );

        let cache_read_cost = self.calculate_tiered_cost(
            usage.cache_read_input_tokens,
            self.cache_read_input_token_cost,
            self.cache_read_input_token_cost_above_200k_tokens,
        );

        input_cost + output_cost + cache_write_cost + cache_read_cost
    }
}

/// Cached pricing data with timestamp
#[derive(Debug, Deserialize, Serialize)]
pub struct PricingCache {
    pub timestamp: i64,
    pub models: HashMap<String, ModelPricing>,
}

/// Semaphore cache for fast statusline rendering
#[derive(Debug, Serialize, Deserialize)]
pub struct Semaphore {
    pub date: String,
    pub last_output: String,
    pub last_update_time: u64,
    pub transcript_path: String,
    pub transcript_mtime: u64,
}

/// 5-hour billing block
#[derive(Debug, Clone)]
pub struct Block {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub cost_usd: f64,
    pub is_active: bool,
    pub hours_remaining: Option<f64>,
}

/// Which limit is critical
#[derive(Debug, PartialEq)]
pub enum LimitType {
    FiveHour,
    SevenDay,
    None,
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

/// API usage data from Anthropic API
#[derive(Debug, Clone)]
pub struct ApiUsageData {
    pub five_hour_percent: f64,
    pub five_hour_resets_at: Option<DateTime<Utc>>,
    pub seven_day_percent: f64,
    pub seven_day_resets_at: Option<DateTime<Utc>>,
    pub seven_day_sonnet_percent: f64,
}

/// Claude configuration from ~/.claude.json
#[derive(Debug, Deserialize)]
pub struct ClaudeConfig {
    #[serde(default = "default_auto_compact", rename = "autoCompactEnabled")]
    pub auto_compact_enabled: bool,
}

fn default_auto_compact() -> bool {
    true
}
