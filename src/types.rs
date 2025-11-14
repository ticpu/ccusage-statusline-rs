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
}

#[derive(Debug, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct Workspace {
    pub current_dir: String,
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

impl ModelPricing {
    pub const THRESHOLD: u64 = 200_000;

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
#[derive(Debug)]
pub struct Block {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub is_active: bool,
}

/// Burn rate information
#[derive(Debug)]
pub struct BurnRate {
    pub cost_per_hour: f64,
    pub tokens_per_minute: u64,
}

/// Context information
#[derive(Debug)]
pub struct ContextInfo {
    pub tokens: u64,
    pub percentage: u32,
}

/// API usage data from claude.ai
#[derive(Debug, Clone)]
pub struct ApiUsageData {
    pub five_hour_percent: f64,
    pub five_hour_resets_at: Option<DateTime<Utc>>,
    pub seven_day_percent: f64,
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
