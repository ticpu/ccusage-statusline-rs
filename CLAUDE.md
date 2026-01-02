# CLAUDE.md

Developer quick-start for ccusage-statusline-rs. Rust implementation of Claude Code usage statusline with live API integration.

## Quick Start

```bash
# Build and test
cargo fmt && cargo clippy --fix --allow-dirty
cargo check --message-format=short
cargo test --message-format=short
cargo build --release

# Install to system and configure Claude
sudo cp target/release/ccusage-statusline-rs /usr/local/bin/
ccusage-statusline-rs install

# CLI subcommands: install, uninstall, test, config (see --help)
ccusage-statusline-rs test       # Quick test with most recent transcript

# Test with real data (piped mode)
echo '{"session_id":"test","transcript_path":"path/to/session.jsonl","model":{"id":"claude-sonnet-4-20250514","display_name":"Claude 3.5 Sonnet"},"workspace":{"current_dir":"/home/user/project"}}' | ./target/release/ccusage-statusline-rs

# Test interactive mode (requires ~/.claude/projects with usage data)
./target/release/ccusage-statusline-rs

# Package for Arch Linux
make package
```

## Code Architecture

```
src/
├── main.rs - Entry point: CLI args, piped/interactive mode
├── types.rs - All structs (HookData, Block, BurnRate, ApiUsageData, etc.)
├── install.rs - Install/uninstall commands for ~/.claude/settings.json
├── pricing.rs - LiteLLM pricing fetch from GitHub (24h cache)
├── blocks.rs - 5-hour billing block logic (dedup by messageId:requestId)
├── cache.rs - Semaphore-based output caching (XDG_RUNTIME_DIR, 30s TTL)
├── format.rs - Output formatting (emojis, colors, directory formatting)
├── claude_binary.rs - Claude Code binary detection and User-Agent extraction
└── api_usage.rs - Anthropic API client (OAuth from ~/.claude/.credentials.json)
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
- Cache: 30s fresh, 5min max stale (returns `ApiUsageResult::StaleCache` after)

**5-Hour Billing Blocks**:
- Floors timestamps to hour boundary
- Deduplicates messages using `{messageId}:{requestId}` hash
- Tracks per-model costs with tiered pricing (200k token threshold)
- Active block = last message within 5 hours

**Performance**:
- Target: <20ms average (15x faster than Node.js warm)
- Caching: Output cache (30s), pricing cache (24h), API cache (30s)
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

**Before every commit**:
```bash
cargo fmt                                    # CRITICAL: CI will fail if not formatted
cargo clippy --fix --allow-dirty --message-format=short
cargo test --message-format=short
```

**Version management** (single source of truth in `Cargo.toml`):
```bash
# 1. Edit Cargo.toml version
# 2. cargo fmt && cargo clippy --fix --allow-dirty
# 3. git commit -m "chore: bump version to X.Y.Z"
# 4. git push
# 5. WAIT for CI to pass on master
# 6. git tag -as vX.Y.Z -m "Release vX.Y.Z"
# 7. git push --tags
# 8. WAIT for Release workflow to complete successfully
# 9. Update AUR package: cd ~/.cache/paru/clone/ccusage-statusline-rs/ && ./update-pkg.sh 2>&1 | grep -v Compiling
#    (Should output the new version; only troubleshoot if it fails)
```

Both `PKGBUILD` and `Makefile` auto-extract version: `grep -Po '^version = "\K[^"]+' Cargo.toml`

**CI/CD**:
- `ci.yml`: Runs on master push/PR (format check, clippy, x86_64 build+test, aarch64 build)
- `release.yml`: Runs on v* tags (creates release, builds x86_64+aarch64 binaries)
- Uses `rustls-tls` (not native-tls) for easier cross-compilation
- aarch64: Sets `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc`, `PKG_CONFIG_ALLOW_CROSS=1`

## Testing

```bash
cargo test --release --message-format=short
```

## Gotchas

- Version is ONLY in Cargo.toml, never edit PKGBUILD/Makefile versions
- DO NOT push tags until CI passes on master
