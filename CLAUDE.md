Claude Code statusline: reconstructs usage from local transcripts, merges it with the claude.ai
usage endpoint, renders one line. Design decisions live in `docs/design-rationale.md`.

## Modules

```
src/
├── main.rs - Subcommands (install/uninstall/test/config), piped vs interactive mode, line assembly
├── types.rs - Shared structs (HookData, ApiUsageData, ActiveBlock, BurnRate, pricing)
├── paths.rs - claude_config_dir, find_claude_paths, iter_jsonl_files
├── config.rs - Element toggles, thresholds, cache TTLs, interactive menu
├── config_migration.rs - Versioned config schema, migrated on load
├── install.rs - statusLine entry in ~/.claude/settings.json
├── pricing.rs - LiteLLM price table from GitHub (24h cache)
├── blocks.rs - 5-hour billing blocks; dedup by {messageId}:{requestId}
├── burn_rate.rs - Burn rate from block cost and usage windows
├── context.rs - Context tokens from the transcript
├── entry_cache.rs - Incremental transcript parse cache (resume by byte offset)
├── cache.rs - Semaphore-file output cache under XDG_RUNTIME_DIR, shared cache IO
├── api_usage.rs - Usage endpoint client (OAuth from ~/.claude/.credentials.json)
├── rate_limits.rs - Merges statusline-stdin rate limits with the endpoint's windows
├── claude_binary.rs - Claude Code binary detection, User-Agent
├── claude_update.rs - Update check (stable/latest channels)
├── http.rs - Shared client, size-limited body reads
├── format.rs - Rendering: emojis, colors, threshold coloring
└── timing.rs - Env-gated phase timing
```

## Constraints

- Render budget is 20ms average in release, 100ms in debug, asserted by `test_performance_under_20ms`
  in `main.rs`. Treat a failure as a defect to fix, never a threshold to raise.
- `CCUSAGE_TIMING=1 ccusage-statusline-rs test` prints per-phase wall time to stderr; the block read
  also reports bytes parsed, which separates "slow phase" from "phase handed too much work".
- Adding, renaming or removing a `StatusElement` needs a `config_migration.rs` step and a
  `CURRENT_VERSION` bump, or existing config files silently lose the setting.
- TLS is rustls, never native-tls: CI and the release build cross-compile to linux x86_64/aarch64,
  windows-gnu x86_64 and macos aarch64.
- Version lives only in `Cargo.toml`; `PKGBUILD` and `Makefile` extract it. Release: `/release`.
- `env -u CLAUDE_CONFIG_DIR` must be its own tool call — the sandbox rejects `env` chained with `&&`.
