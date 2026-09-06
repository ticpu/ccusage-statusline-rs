use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Per-TTL split of `cache_creation_input_tokens`. Long-TTL writes cost more, and the
/// flat total cannot distinguish them.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CacheCreationBreakdown {
    #[serde(default)]
    pub ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    pub ephemeral_1h_input_tokens: u64,
}

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

impl HookData {
    /// Stand-in for the modes that render without Claude Code on the other end of a pipe.
    pub fn placeholder(display_name: &str, workspace: Option<Workspace>) -> Self {
        Self {
            session_id: String::new(),
            transcript_path: String::new(),
            model: ModelInfo {
                id: None,
                display_name: display_name.to_string(),
            },
            workspace,
            context_window: None,
            rate_limits: None,
        }
    }
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
