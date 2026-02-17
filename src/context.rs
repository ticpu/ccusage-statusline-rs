use crate::{
    paths::home_dir,
    types::{ClaudeConfig, ContextInfo, UsageData},
};
use anyhow::Result;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};

const COMPACTED_CONTEXT_LIMIT: u64 = 155_000;
const FULL_CONTEXT_LIMIT: u64 = 200_000;

fn get_context_limit() -> u64 {
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

pub fn calculate_context_tokens(transcript_path: &str) -> Result<Option<ContextInfo>> {
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
    let context_limit = get_context_limit();
    let percentage = ((total_tokens as f64 / context_limit as f64) * 100.0).min(100.0) as u32;

    Ok(Some(ContextInfo {
        tokens: total_tokens,
        percentage,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

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
