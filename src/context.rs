use crate::{
    paths::claude_config_json_path,
    types::{ContextInfo, ContextWindowData, HookData, UsageData},
};
use anyhow::Result;
use serde::Deserialize;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, ErrorKind, IsTerminal};

// Reconstruction of Claude Code's managed context window. The statusline payload only
// carries the raw model window, but what the user actually hits is auto-compact, which
// fires against a narrower one. Values track Claude Code 2.1.224; when a release moves
// them the whole group moves together.
const DEFAULT_MODEL_WINDOW: u64 = 200_000;
const EXTENDED_MODEL_WINDOW: u64 = 1_000_000;
/// Held back for the response on every request.
const OUTPUT_RESERVE: u64 = 20_000;
/// Further headroom auto-compact keeps below the usable window before it triggers.
const COMPACTION_HEADROOM: u64 = 13_000;

/// Models whose auto-compact window is narrowed below their raw model window by
/// policy. The narrowing only applies while auto-compact is on, which is why turning
/// it off gains more window on these than on models absent from the table.
const AUTO_COMPACT_WINDOWS: &[(&str, u64)] = &[("claude-sonnet-5", 967_000)];

/// Claude configuration from ~/.claude.json
#[derive(Debug, Deserialize)]
struct ClaudeConfig {
    #[serde(default = "default_auto_compact", rename = "autoCompactEnabled")]
    auto_compact_enabled: bool,
}

fn default_auto_compact() -> bool {
    true
}

pub fn calculate_context(hook_data: &HookData) -> Result<Option<ContextInfo>> {
    let model_id = hook_data
        .model
        .id
        .as_deref();
    let auto_compact = auto_compact_enabled();

    if let Some(cw) = &hook_data.context_window
        && let Some(info) = context_from_window(cw, model_id, auto_compact)
    {
        return Ok(Some(info));
    }

    calculate_context_from_transcript(&hook_data.transcript_path, model_id, auto_compact)
}

fn to_info(tokens: u64, limit: u64) -> ContextInfo {
    ContextInfo {
        tokens,
        percentage: ((tokens as f64 / limit as f64) * 100.0).min(100.0) as u32,
    }
}

fn context_from_window(
    cw: &ContextWindowData,
    model_id: Option<&str>,
    auto_compact: bool,
) -> Option<ContextInfo> {
    // `total_input_tokens` is the sum Claude Code derives its own percentage from;
    // taking the count from one quantity and the percentage from another renders one
    // element whose halves disagree.
    let tokens = cw
        .total_input_tokens
        .or_else(|| {
            cw.current_usage
                .as_ref()
                .map(|u| u.context_tokens())
        })?;

    Some(to_info(
        tokens,
        effective_context_limit(model_id, cw.context_window_size, auto_compact),
    ))
}

fn is_1m_context_model(model_id: &str) -> bool {
    // The `[1m]` suffix is itself the 1M-context marker: models that only reach 1M
    // in that mode (Sonnet 4.5) carry it, so stripping it loses the answer.
    if model_id.ends_with("[1m]") {
        return true;
    }
    let base = model_id
        .split('[')
        .next()
        .unwrap_or(model_id);
    base.starts_with("claude-opus-4-6") || base.starts_with("claude-sonnet-4-6")
}

/// Raw model window, preferring what Claude Code reported over what we can infer.
fn model_window(model_id: Option<&str>, reported: Option<u64>) -> u64 {
    if let Some(size) = reported.filter(|s| *s > 0) {
        return size;
    }
    match model_id {
        Some(id) if is_1m_context_model(id) => EXTENDED_MODEL_WINDOW,
        _ => DEFAULT_MODEL_WINDOW,
    }
}

fn auto_compact_window(model_id: Option<&str>) -> Option<u64> {
    let id = model_id?;
    AUTO_COMPACT_WINDOWS
        .iter()
        .find(|(prefix, _)| id.starts_with(prefix))
        .map(|(_, window)| *window)
}

/// Token count at which the session stops growing — the auto-compact trigger when
/// enabled, otherwise the plain usable window.
///
/// Reporting against the raw model window instead would read comfortable right up to
/// the moment a session compacts.
fn effective_context_limit(
    model_id: Option<&str>,
    reported_window: Option<u64>,
    auto_compact: bool,
) -> u64 {
    let raw = model_window(model_id, reported_window);

    let managed = if auto_compact {
        auto_compact_window(model_id).map_or(raw, |policy| raw.min(policy))
    } else {
        raw
    };

    let usable = managed.saturating_sub(OUTPUT_RESERVE);
    if auto_compact {
        usable.saturating_sub(COMPACTION_HEADROOM)
    } else {
        usable
    }
    .max(1)
}

fn auto_compact_enabled() -> bool {
    let config_path = match claude_config_json_path() {
        Ok(p) => p,
        Err(e) => {
            if std::io::stderr().is_terminal() {
                eprintln!("Context limit: could not determine config path: {e:#}");
            }
            return default_auto_compact();
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

    config.map_or_else(default_auto_compact, |c| c.auto_compact_enabled)
}

fn calculate_context_from_transcript(
    transcript_path: &str,
    model_id: Option<&str>,
    auto_compact: bool,
) -> Result<Option<ContextInfo>> {
    let file = match File::open(transcript_path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            if std::io::stderr().is_terminal() {
                eprintln!("Context: cannot read {transcript_path}: {e:#}");
            }
            return Ok(None);
        }
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
            last_tokens = Some(
                entry
                    .message
                    .usage
                    .context_tokens(),
            );
        }
    }

    let total_tokens = last_tokens.unwrap_or(0);
    // No payload here, so the raw window has to be inferred from the model id.
    let limit = effective_context_limit(model_id, None, auto_compact);

    Ok(Some(to_info(total_tokens, limit)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelInfo, UsageTokens};
    use std::fs;
    use std::io::Write;

    fn write_jsonl(path: &std::path::Path, lines: &[&str]) {
        let mut f = fs::File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    // effective_context_limit: the five states from the Claude Code window reconstruction.
    // Each row is (model, raw window reported, auto-compact, expected limit).

    #[test]
    fn test_limit_200k_model_auto_compact_on() {
        assert_eq!(
            effective_context_limit(Some("claude-opus-5"), Some(200_000), true),
            167_000
        );
    }

    #[test]
    fn test_limit_200k_model_auto_compact_off() {
        assert_eq!(
            effective_context_limit(Some("claude-opus-5"), Some(200_000), false),
            180_000
        );
    }

    /// Sonnet 5's auto-compact window is narrowed by policy below its raw window, so
    /// turning auto-compact off gains far more here than on a model without an entry.
    #[test]
    fn test_limit_policy_narrowed_model() {
        assert_eq!(
            effective_context_limit(Some("claude-sonnet-5"), Some(1_000_000), true),
            934_000
        );
        assert_eq!(
            effective_context_limit(Some("claude-sonnet-5"), Some(1_000_000), false),
            980_000
        );
    }

    /// A 1M-entitled model absent from the policy table keeps its whole window.
    #[test]
    fn test_limit_1m_entitled_model_not_narrowed() {
        assert_eq!(
            effective_context_limit(Some("claude-opus-5"), Some(1_000_000), true),
            967_000
        );
    }

    #[test]
    fn test_limit_infers_window_when_payload_absent() {
        assert_eq!(
            effective_context_limit(Some("claude-sonnet-4-5-20250929[1m]"), None, true),
            967_000
        );
        assert_eq!(
            effective_context_limit(Some("claude-sonnet-4-5-20250929"), None, true),
            167_000
        );
        assert_eq!(effective_context_limit(None, None, true), 167_000);
    }

    /// The reported window wins over inference: entitlement can be revoked at runtime,
    /// and only Claude Code knows that.
    #[test]
    fn test_limit_reported_window_overrides_model_id() {
        assert_eq!(
            effective_context_limit(Some("claude-opus-4-6"), Some(200_000), true),
            167_000
        );
    }

    #[test]
    fn test_context_from_window_prefers_total_input_tokens() {
        use crate::types::ContextWindowData;
        let cw = ContextWindowData {
            total_input_tokens: Some(42_000),
            context_window_size: Some(1_000_000),
            current_usage: Some(UsageTokens {
                input_tokens: 8_500,
                output_tokens: 0,
                cache_creation_input_tokens: 5_000,
                cache_read_input_tokens: 2_000,
                cache_creation: None,
            }),
        };
        let info = context_from_window(&cw, Some("claude-opus-5"), true).unwrap();
        // The count and the percentage must describe the same quantity.
        assert_eq!(info.tokens, 42_000);
        assert_eq!(info.percentage, (42_000 * 100) / 967_000);
    }

    #[test]
    fn test_context_from_window_falls_back_to_current_usage() {
        use crate::types::ContextWindowData;
        let cw = ContextWindowData {
            total_input_tokens: None,
            context_window_size: Some(200_000),
            current_usage: Some(UsageTokens {
                input_tokens: 8_500,
                output_tokens: 0,
                cache_creation_input_tokens: 5_000,
                cache_read_input_tokens: 2_000,
                cache_creation: None,
            }),
        };
        let info = context_from_window(&cw, Some("claude-opus-5"), true).unwrap();
        assert_eq!(info.tokens, 15_500);
    }

    #[test]
    fn test_context_from_window_without_tokens_is_none() {
        use crate::types::ContextWindowData;
        let cw = ContextWindowData {
            total_input_tokens: None,
            context_window_size: Some(200_000),
            current_usage: None,
        };
        assert!(context_from_window(&cw, Some("claude-opus-5"), true).is_none());
    }

    #[test]
    fn test_is_1m_context_model() {
        assert!(is_1m_context_model("claude-opus-4-6"));
        assert!(is_1m_context_model("claude-opus-4-6-20260205"));
        assert!(is_1m_context_model("claude-opus-4-6[1m]"));
        assert!(is_1m_context_model("claude-sonnet-4-6"));
        assert!(is_1m_context_model("claude-sonnet-4-5-20250929[1m]"));

        assert!(!is_1m_context_model("claude-opus-4-5-20251101"));
        assert!(!is_1m_context_model("claude-sonnet-4-5-20250929"));
    }

    #[test]
    fn test_calculate_context_uses_window_data() {
        use crate::types::{ContextWindowData, HookData};
        let hook = HookData {
            session_id: "test".into(),
            transcript_path: "/nonexistent".into(),
            model: ModelInfo {
                id: Some("claude-opus-4-6".into()),
                display_name: "Opus 4.6 (1M context)".into(),
            },
            workspace: None,
            context_window: Some(ContextWindowData {
                total_input_tokens: Some(42_000),
                context_window_size: Some(1_000_000),
                current_usage: None,
            }),
            rate_limits: None,
        };
        let info = calculate_context(&hook)
            .unwrap()
            .unwrap();
        assert_eq!(info.tokens, 42_000);
    }

    #[test]
    fn test_transcript_compacted_limit() {
        let dir = crate::paths::test_scratch_dir("ctx-compacted");
        let path = dir.join("session.jsonl");
        // tokens: input=10, cache_creation=500, cache_read=95000 → total=95510
        write_jsonl(
            &path,
            &[
                r#"{"timestamp":"2024-01-01T00:00:00Z","message":{"usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":500,"cache_read_input_tokens":95000}}}"#,
            ],
        );

        // 200k model with auto-compact on → 167_000 limit
        let info = calculate_context_from_transcript(
            path.to_str()
                .unwrap(),
            Some("claude-sonnet-4-5"),
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(info.tokens, 95_510);
        assert_eq!(info.percentage, (95_510 * 100) / 167_000);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_transcript_1m_model_limit() {
        let dir = crate::paths::test_scratch_dir("ctx-1m");
        let path = dir.join("session.jsonl");
        write_jsonl(
            &path,
            &[
                r#"{"timestamp":"2024-01-01T00:00:00Z","message":{"usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":500,"cache_read_input_tokens":95000}}}"#,
            ],
        );

        // 1M model, no policy narrowing → 967_000 limit
        let info = calculate_context_from_transcript(
            path.to_str()
                .unwrap(),
            Some("claude-opus-4-6"),
            true,
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
        let dir = crate::paths::test_scratch_dir("ctx-multi");
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
            true,
        )
        .unwrap()
        .unwrap();
        // last entry wins: input=2000
        assert_eq!(info.tokens, 2_000);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_transcript_empty_file() {
        let dir = crate::paths::test_scratch_dir("ctx-empty");
        let path = dir.join("session.jsonl");
        write_jsonl(&path, &[]);

        let info = calculate_context_from_transcript(
            path.to_str()
                .unwrap(),
            Some("claude-sonnet-4-5"),
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(info.tokens, 0);
        assert_eq!(info.percentage, 0);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_transcript_nonexistent_returns_none() {
        let info = calculate_context_from_transcript("/nonexistent/path/session.jsonl", None, true)
            .unwrap();
        assert!(info.is_none());
    }
}
