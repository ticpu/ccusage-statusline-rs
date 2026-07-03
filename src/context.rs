use crate::{
    paths::claude_config_json_path,
    types::{ClaudeConfig, ContextInfo, ContextWindowData, HookData, UsageData},
};
use anyhow::Result;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, ErrorKind, IsTerminal};

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

/// Pure decision core: model id + parsed config → context limit constant.
fn select_context_limit(model_id: Option<&str>, config: Option<&ClaudeConfig>) -> u64 {
    if let Some(id) = model_id
        && is_1m_context_model(id)
    {
        return EXTENDED_CONTEXT_LIMIT;
    }

    match config {
        Some(c) if !c.auto_compact_enabled => FULL_CONTEXT_LIMIT,
        _ => COMPACTED_CONTEXT_LIMIT,
    }
}

fn get_context_limit(model_id: Option<&str>) -> u64 {
    let config_path = match claude_config_json_path() {
        Ok(p) => p,
        Err(e) => {
            if std::io::stderr().is_terminal() {
                eprintln!("Context limit: could not determine config path: {e:#}");
            }
            return select_context_limit(model_id, None);
        }
    };

    let config = match fs::read_to_string(&config_path) {
        Ok(content) => match serde_json::from_str::<ClaudeConfig>(&content) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                if std::io::stderr().is_terminal() {
                    eprintln!(
                        "Context limit: failed to parse {}: {e:#}",
                        config_path.display()
                    );
                }
                None
            }
        },
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => {
            if std::io::stderr().is_terminal() {
                eprintln!(
                    "Context limit: failed to read {}: {e:#}",
                    config_path.display()
                );
            }
            None
        }
    };

    select_context_limit(model_id, config.as_ref())
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
    use std::fs;
    use std::io::Write;

    fn write_jsonl(path: &std::path::Path, lines: &[&str]) {
        let mut f = fs::File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    fn compacted_config() -> ClaudeConfig {
        serde_json::from_str(r#"{"autoCompactEnabled": true}"#).unwrap()
    }

    fn full_config() -> ClaudeConfig {
        serde_json::from_str(r#"{"autoCompactEnabled": false}"#).unwrap()
    }

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
            rate_limits: None,
        };
        let info = calculate_context(&hook)
            .unwrap()
            .unwrap();
        assert_eq!(info.percentage, 4);
        assert_eq!(info.tokens, 42_000);
    }

    // select_context_limit: pure function tests — no IO, no real home directory

    #[test]
    fn test_select_limit_1m_model() {
        assert_eq!(
            select_context_limit(Some("claude-sonnet-4-6"), None),
            EXTENDED_CONTEXT_LIMIT
        );
        assert_eq!(
            select_context_limit(Some("claude-opus-4-6"), Some(&compacted_config())),
            EXTENDED_CONTEXT_LIMIT
        );
    }

    #[test]
    fn test_select_limit_1m_overrides_full_config() {
        assert_eq!(
            select_context_limit(Some("claude-opus-4-6"), Some(&full_config())),
            EXTENDED_CONTEXT_LIMIT
        );
    }

    #[test]
    fn test_select_limit_auto_compact_enabled() {
        assert_eq!(
            select_context_limit(Some("claude-sonnet-4-5"), Some(&compacted_config())),
            COMPACTED_CONTEXT_LIMIT
        );
    }

    #[test]
    fn test_select_limit_auto_compact_disabled() {
        assert_eq!(
            select_context_limit(Some("claude-sonnet-4-5"), Some(&full_config())),
            FULL_CONTEXT_LIMIT
        );
    }

    #[test]
    fn test_select_limit_no_config_defaults_compacted() {
        assert_eq!(
            select_context_limit(Some("claude-sonnet-4-5"), None),
            COMPACTED_CONTEXT_LIMIT
        );
        assert_eq!(select_context_limit(None, None), COMPACTED_CONTEXT_LIMIT);
    }

    // calculate_context_from_transcript: drive production code with temp JSONL fixtures

    #[test]
    fn test_transcript_compacted_limit() {
        let dir = std::env::temp_dir().join("ccusage-test-ctx-compacted");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        // tokens: input=10, cache_creation=500, cache_read=95000 → total=95510
        write_jsonl(
            &path,
            &[
                r#"{"timestamp":"2024-01-01T00:00:00Z","message":{"usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":500,"cache_read_input_tokens":95000}}}"#,
            ],
        );

        // compacted config → 155_000 limit; 95510/155000 = 61%
        let limit = select_context_limit(Some("claude-sonnet-4-5"), Some(&compacted_config()));
        assert_eq!(limit, COMPACTED_CONTEXT_LIMIT);

        let info = calculate_context_from_transcript(
            path.to_str()
                .unwrap(),
            Some("claude-sonnet-4-5"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(info.tokens, 95_510);
        // percentage depends on get_context_limit which reads real disk; check tokens only here.
        // Limit selection is covered by test_select_limit_* above.
        assert!(info.percentage <= 100);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_transcript_1m_model_limit() {
        let dir = std::env::temp_dir().join("ccusage-test-ctx-1m");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        write_jsonl(
            &path,
            &[
                r#"{"timestamp":"2024-01-01T00:00:00Z","message":{"usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":500,"cache_read_input_tokens":95000}}}"#,
            ],
        );

        // 1M model → 1_000_000 limit; 95510/1000000 = 9%
        let limit = select_context_limit(Some("claude-opus-4-6"), None);
        assert_eq!(limit, EXTENDED_CONTEXT_LIMIT);

        let info = calculate_context_from_transcript(
            path.to_str()
                .unwrap(),
            Some("claude-opus-4-6"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(info.tokens, 95_510);
        // 95510 / 1_000_000 * 100 = 9%
        assert_eq!(info.percentage, 9);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_transcript_multiple_entries_last_wins() {
        let dir = std::env::temp_dir().join("ccusage-test-ctx-multi");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        write_jsonl(
            &path,
            &[
                r#"{"timestamp":"2024-01-01T00:00:00Z","message":{"usage":{"input_tokens":1000,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
                r#"{"timestamp":"2024-01-01T00:01:00Z","message":{"usage":{"input_tokens":2000,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            ],
        );

        let info = calculate_context_from_transcript(
            path.to_str()
                .unwrap(),
            Some("claude-opus-4-6"),
        )
        .unwrap()
        .unwrap();
        // last entry: input=2000, total=2000; 2000/1_000_000*100 = 0%
        assert_eq!(info.tokens, 2_000);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_transcript_empty_file() {
        let dir = std::env::temp_dir().join("ccusage-test-ctx-empty");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        write_jsonl(&path, &[]);

        let info = calculate_context_from_transcript(
            path.to_str()
                .unwrap(),
            Some("claude-sonnet-4-5"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(info.tokens, 0);
        assert_eq!(info.percentage, 0);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_transcript_nonexistent_returns_none() {
        let info =
            calculate_context_from_transcript("/nonexistent/path/session.jsonl", None).unwrap();
        assert!(info.is_none());
    }
}
