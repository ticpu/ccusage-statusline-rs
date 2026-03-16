use crate::{
    paths::home_dir,
    types::{ClaudeConfig, ContextInfo, ContextWindowData, HookData, UsageData},
};
use anyhow::Result;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};

const COMPACTED_CONTEXT_LIMIT: u64 = 155_000;
const FULL_CONTEXT_LIMIT: u64 = 200_000;
const EXTENDED_CONTEXT_LIMIT: u64 = 1_000_000;

pub fn calculate_context(hook_data: &HookData) -> Result<Option<ContextInfo>> {
    if let Some(cw) = &hook_data.context_window
        && let Some(info) = context_from_window(cw)
    {
        return Ok(Some(info));
    }

    calculate_context_from_transcript(
        &hook_data.transcript_path,
        hook_data
            .model
            .id
            .as_deref(),
    )
}

fn context_from_window(cw: &ContextWindowData) -> Option<ContextInfo> {
    let pct = cw.used_percentage?;

    let tokens = if let Some(usage) = &cw.current_usage {
        usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens
    } else {
        cw.total_input_tokens
            .unwrap_or(0)
    };

    Some(ContextInfo {
        tokens,
        percentage: (pct as u32).min(100),
    })
}

fn is_1m_context_model(model_id: &str) -> bool {
    let base = model_id
        .split('[')
        .next()
        .unwrap_or(model_id);
    base.starts_with("claude-opus-4-6") || base.starts_with("claude-sonnet-4-6")
}

fn get_context_limit(model_id: Option<&str>) -> u64 {
    if let Some(id) = model_id
        && is_1m_context_model(id)
    {
        return EXTENDED_CONTEXT_LIMIT;
    }

    let home = match home_dir() {
        Ok(h) => h,
        Err(_) => return COMPACTED_CONTEXT_LIMIT,
    };

    let config_path = home.join(".claude.json");

    match fs::read_to_string(&config_path) {
        Ok(content) => match serde_json::from_str::<ClaudeConfig>(&content) {
            Ok(config) => {
                if config.auto_compact_enabled {
                    COMPACTED_CONTEXT_LIMIT
                } else {
                    FULL_CONTEXT_LIMIT
                }
            }
            Err(_) => COMPACTED_CONTEXT_LIMIT,
        },
        Err(_) => COMPACTED_CONTEXT_LIMIT,
    }
}

fn calculate_context_from_transcript(
    transcript_path: &str,
    model_id: Option<&str>,
) -> Result<Option<ContextInfo>> {
    let file = match File::open(transcript_path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };

    let reader = BufReader::new(file);
    let mut last_tokens: Option<u64> = None;

    for line in reader.lines() {
        let line = line?;
        if line
            .trim()
            .is_empty()
        {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<UsageData>(&line) {
            let context = entry
                .message
                .usage
                .input_tokens
                + entry
                    .message
                    .usage
                    .cache_creation_input_tokens
                + entry
                    .message
                    .usage
                    .cache_read_input_tokens;
            last_tokens = Some(context);
        }
    }

    let total_tokens = last_tokens.unwrap_or(0);
    let context_limit = get_context_limit(model_id);
    let percentage = ((total_tokens as f64 / context_limit as f64) * 100.0).min(100.0) as u32;

    Ok(Some(ContextInfo {
        tokens: total_tokens,
        percentage,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContextUsage, ModelInfo};

    #[test]
    fn test_context_from_window_1m() {
        let cw = ContextWindowData {
            used_percentage: Some(4.2),
            total_input_tokens: Some(42_000),
            current_usage: Some(ContextUsage {
                input_tokens: 8_500,
                cache_creation_input_tokens: 5_000,
                cache_read_input_tokens: 2_000,
            }),
        };
        let info = context_from_window(&cw).unwrap();
        assert_eq!(info.tokens, 15_500);
        assert_eq!(info.percentage, 4);
    }

    #[test]
    fn test_context_from_window_200k() {
        let cw = ContextWindowData {
            used_percentage: Some(47.5),
            total_input_tokens: Some(95_000),
            current_usage: None,
        };
        let info = context_from_window(&cw).unwrap();
        assert_eq!(info.tokens, 95_000);
        assert_eq!(info.percentage, 47);
    }

    #[test]
    fn test_context_from_window_no_percentage() {
        let cw = ContextWindowData {
            used_percentage: None,
            total_input_tokens: Some(42_000),
            current_usage: None,
        };
        assert!(context_from_window(&cw).is_none());
    }

    #[test]
    fn test_context_from_window_defaults() {
        let cw = ContextWindowData {
            used_percentage: Some(10.0),
            total_input_tokens: None,
            current_usage: None,
        };
        let info = context_from_window(&cw).unwrap();
        assert_eq!(info.tokens, 0);
        assert_eq!(info.percentage, 10);
    }

    #[test]
    fn test_is_1m_context_model() {
        assert!(is_1m_context_model("claude-opus-4-6"));
        assert!(is_1m_context_model("claude-opus-4-6-20260205"));
        assert!(is_1m_context_model("claude-opus-4-6[1m]"));
        assert!(is_1m_context_model("claude-sonnet-4-6"));
        assert!(is_1m_context_model("claude-sonnet-4-6[1m]"));

        assert!(!is_1m_context_model("claude-opus-4-5-20251101"));
        assert!(!is_1m_context_model("claude-sonnet-4-5-20250929"));
        assert!(!is_1m_context_model("claude-haiku-4-5-20251001"));
        assert!(!is_1m_context_model("claude-sonnet-4-20250514"));
    }

    #[test]
    fn test_calculate_context_uses_window_data() {
        let hook = HookData {
            session_id: "test".into(),
            transcript_path: "/nonexistent".into(),
            model: ModelInfo {
                id: Some("claude-opus-4-6".into()),
                display_name: "Opus 4.6 (1M context)".into(),
            },
            workspace: None,
            context_window: Some(ContextWindowData {
                used_percentage: Some(4.2),
                total_input_tokens: Some(42_000),
                current_usage: None,
            }),
        };
        let info = calculate_context(&hook)
            .unwrap()
            .unwrap();
        assert_eq!(info.percentage, 4);
        assert_eq!(info.tokens, 42_000);
    }

    #[test]
    fn test_context_calculation_with_caching_compacted() {
        let tokens = 10 + 500 + 95000;
        let percentage =
            ((tokens as f64 / COMPACTED_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(tokens, 95510);
        assert_eq!(percentage, 61);
    }

    #[test]
    fn test_context_calculation_with_caching_full() {
        let tokens = 10 + 500 + 95000;
        let percentage = ((tokens as f64 / FULL_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(tokens, 95510);
        assert_eq!(percentage, 47);
    }

    #[test]
    fn test_context_calculation_1m() {
        let tokens = 10 + 500 + 95000;
        let percentage =
            ((tokens as f64 / EXTENDED_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(tokens, 95510);
        assert_eq!(percentage, 9);
    }

    #[test]
    fn test_context_calculation_without_caching_compacted() {
        let tokens = 1000;
        let percentage =
            ((tokens as f64 / COMPACTED_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(tokens, 1000);
        assert_eq!(percentage, 0);
    }

    #[test]
    fn test_context_calculation_without_caching_full() {
        let tokens = 1000;
        let percentage = ((tokens as f64 / FULL_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(tokens, 1000);
        assert_eq!(percentage, 0);
    }

    #[test]
    fn test_context_calculation_capped_compacted() {
        let tokens = 199_000u64;
        let percentage =
            ((tokens as f64 / COMPACTED_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(percentage, 100);
    }

    #[test]
    fn test_context_calculation_capped_full() {
        let tokens = 250_000u64;
        let percentage = ((tokens as f64 / FULL_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(percentage, 100);
    }
}
