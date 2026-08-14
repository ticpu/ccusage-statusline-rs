# CLAUDE.md

Developer quick-start for ccusage-statusline-rs. Rust implementation of Claude Code usage statusline with live API integration.

## Quick Start

```bash
# CLI subcommands: install, uninstall, test, config (see --help)
ccusage-statusline-rs test       # Quick test with most recent transcript

# Test with real data (piped mode)
echo '{"session_id":"test","transcript_path":"path/to/session.jsonl","model":{"id":"claude-sonnet-4-20250514","display_name":"Claude 3.5 Sonnet"},"workspace":{"current_dir":"/home/user/project"}}' | ./target/release/ccusage-statusline-rs

# Test interactive mode (requires ~/.claude/projects with usage data)
./target/release/ccusage-statusline-rs
```

## Code Architecture

```
src/
├── main.rs - Entry point: CLI args, piped/interactive mode, statusline assembly
├── types.rs - All structs (HookData, Block, BurnRate, TokenPrices, ApiUsageData, etc.)
├── paths.rs - Shared path helpers (home_dir, find_claude_paths, iter_jsonl_files)
├── install.rs - Install/uninstall commands for ~/.claude/settings.json
├── config.rs - Statusline element configuration (enable/disable, thresholds)
├── pricing.rs - LiteLLM pricing fetch from GitHub (24h cache)
├── blocks.rs - 5-hour billing block logic (dedup by messageId:requestId)
├── burn_rate.rs - Burn rate calculation from block + API usage data
├── context.rs - Context token calculation from transcript JSONL
├── cache.rs - Semaphore-based output caching (XDG_RUNTIME_DIR), shared cache IO
├── format.rs - Output formatting (emojis, colors, directory formatting)
├── claude_binary.rs - Claude Code binary detection and User-Agent extraction
├── claude_update.rs - Update availability check (stable/latest channels)
├── api_usage.rs - Anthropic API client (OAuth from ~/.claude/.credentials.json)
├── entry_cache.rs - Incremental transcript parse cache (resume by byte offset)
└── timing.rs - Env-gated phase timing (CCUSAGE_TIMING=1)
```

**Data Flow**:
1. Parse CLI args (install/uninstall subcommands or default mode)
2. Input: JSON from stdin (with workspace.current_dir) or detect interactive mode
3. Load pricing from cache or fetch from GitHub
4. Try fetch live usage from claude.ai API (silent failure)
5. Scan ~/.claude/projects for usage JSONL files
6. Calculate costs, blocks, burn rate from local data
7. Use API reset time if available (more accurate than local)
8. Format directory path (replace $HOME with ~, add green color)
9. Output: `🤖 Model | 💰 Block | 🔥 Burn | 🧠 Context | 📊 API (if available) ~/directory`

## Key Implementation Details

**API Usage** (`api_usage.rs`):
- OAuth token from `~/.claude/.credentials.json`
- Cache TTLs are config-driven; see `CacheSettings` defaults in `config.rs`

**5-Hour Billing Blocks**:
- Floors timestamps to hour boundary
- Deduplicates messages using `{messageId}:{requestId}` hash
- Long-context tier selected per request from prompt size, applied to all categories
- Cache writes priced by TTL (1h writes cost more than 5m)
- Boundary comes from the API/stdin reset time when known; derived by gap otherwise

**Profiling**: `CCUSAGE_TIMING=1 ccusage-statusline-rs test` prints per-phase wall
time to stderr (`api`, `pricing`, `update`, `block.scan`, `block.read`, `block`,
`context`). `block.read` also reports bytes parsed, which separates "slow phase"
from "phase handed too much work".

**Performance**:
- Target: <20ms average in release mode (STRICTLY ENFORCED by CI)
- This is 15x faster than Node.js warm (120ms)
- Failing the performance test is NOT acceptable - investigate and fix before committing
- Caching: output, API usage and transcript-entry caches (TTLs in `config.rs`), pricing (24h)
- Early returns: Skip processing if cache hit

**Install/Uninstall Commands**:
- `install` subcommand: Automatically configures `~/.claude/settings.json`
  - Checks if file exists (error if not: "run Claude Code once first")
  - Parses JSON, checks for existing statusLine config
  - If exists: displays current config, prompts y/n to overwrite
  - Writes simple config: `{"type": "command", "command": "/path/to/binary"}`
  - Uses `std::env::current_exe()` to get binary path automatically
- `uninstall` subcommand: Removes statusLine configuration
  - Parses JSON, removes statusLine key
  - Writes back to file
- No bash/jq/sed dependencies - all logic in Rust
- Directory formatting done by binary (parses workspace.current_dir from JSON)

## Development Workflow

**Version management**: single source of truth in `Cargo.toml`; both `PKGBUILD` and `Makefile` auto-extract it (`grep -Po '^version = "\K[^"]+' Cargo.toml`). Release process: `/release` (`.claude/commands/release.md`).

**CI/CD**:
- `ci.yml`: Runs on master push/PR (format check, clippy, x86_64 build+test, aarch64 build)
- `release.yml`: Runs on v* tags (creates release, builds x86_64+aarch64 binaries)
- Uses `rustls-tls` (not native-tls) for easier cross-compilation
- aarch64: Sets `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc`, `PKG_CONFIG_ALLOW_CROSS=1`

## Gotchas

- Version is ONLY in Cargo.toml, never edit PKGBUILD/Makefile versions
- DO NOT push tags until CI passes on master
- When testing with `env -u CLAUDE_CONFIG_DIR`, do not chain with `&&` — the Claude Code sandbox requires separate tool calls for `env` commands
