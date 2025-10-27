# CLAUDE.md

Developer quick-start for ccusage-statusline-rs. Rust implementation of Claude Code usage statusline with live API integration.

## Quick Start

```bash
# Build and test
cargo fmt && cargo clippy --fix --allow-dirty
cargo check --message-format=short
cargo test --message-format=short
cargo build --release

# Test with real data (piped mode)
echo '{"session_id":"test","transcript_path":"path/to/session.jsonl","model":{"id":"claude-sonnet-4-20250514","display_name":"Claude 3.5 Sonnet"}}' | ./target/release/ccusage-statusline-rs

# Test interactive mode (requires ~/.claude/projects with usage data)
./target/release/ccusage-statusline-rs

# Install locally
make package
```

## Code Architecture

```
src/
├── main.rs       - Entry point: piped mode vs interactive mode detection
├── types.rs      - All structs (HookData, Block, BurnRate, ApiUsageData, etc.)
├── pricing.rs    - LiteLLM pricing fetch from GitHub (24h cache)
├── blocks.rs     - 5-hour billing block logic (dedup by messageId:requestId)
├── cache.rs      - Semaphore-based output caching (XDG_RUNTIME_DIR, 30s TTL)
├── format.rs     - Output formatting (emojis, colors, number formatting)
├── firefox.rs    - Firefox cookie extraction (immutable=1 SQLite, userID matching)
└── api_usage.rs  - Claude.ai live API client (30s cache, graceful fallback)
```

**Data Flow**:
1. Input: JSONL from stdin or detect interactive mode
2. Load pricing from cache or fetch from GitHub
3. Try fetch live usage from claude.ai API (silent failure)
4. Scan ~/.claude/projects for usage JSONL files
5. Calculate costs, blocks, burn rate from local data
6. Use API reset time if available (more accurate than local)
7. Output: `🤖 Model | 💰 Block | 🔥 Burn | 🧠 Context | 📊 API (if available)`

## Key Implementation Details

**Firefox Cookie Extraction**:
- Uses `file:///path/to/cookies.sqlite?immutable=1` to read locked DB
- Matches `~/.claude/claude.json` userID to Firefox profile (searches cookies for ajs_user_id match)
- Falls back to most recently modified profile
- Extracts: `sessionKey`, `lastActiveOrg` only (minimal cookies needed)

**Claude.ai API Integration**:
- Endpoint: `https://claude.ai/api/organizations/{org}/usage`
- Implementation: Uses libcurl via `curl` crate (Cloudflare blocks reqwest/rustls TLS fingerprint)
- Headers: User-Agent (extracted from Firefox binary version), Cookie (sessionKey + lastActiveOrg)
- Response: `{five_hour: {utilization: 5, resets_at: "..."}, seven_day: {utilization: 25, ...}}`
- Caching: 30s in-memory (Mutex<Option<CachedResponse>>)
- Errors: All API failures silent (stderr only), graceful fallback to local data

**5-Hour Billing Blocks**:
- Floors timestamps to hour boundary
- Deduplicates messages using `{messageId}:{requestId}` hash
- Tracks per-model costs with tiered pricing (200k token threshold)
- Active block = last message within 5 hours

**Performance**:
- Target: <20ms average (15x faster than Node.js warm)
- Caching: Output cache (30s), pricing cache (24h), API cache (30s)
- Early returns: Skip processing if cache hit

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
```

Both `PKGBUILD` and `Makefile` auto-extract version: `grep -Po '^version = "\K[^"]+' Cargo.toml`

**CI/CD**:
- `ci.yml`: Runs on master push/PR (format check, clippy, x86_64 build+test, aarch64 build)
- `release.yml`: Runs on v* tags (creates release, builds x86_64+aarch64 binaries)
- Uses `rustls-tls` (not native-tls) for easier cross-compilation
- aarch64: Sets `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc`, `PKG_CONFIG_ALLOW_CROSS=1`

## Testing

**Unit tests**: `cargo test`

**Integration testing**:
```bash
# With real transcript
TRANSCRIPT=$(find ~/.claude/projects -name "*.jsonl" | head -1)
echo "{\"session_id\":\"test\",\"transcript_path\":\"$TRANSCRIPT\",\"model\":{\"id\":\"claude-sonnet-4-20250514\",\"display_name\":\"Claude 3.5 Sonnet\"}}" | cargo run

# Interactive mode (requires ~/.claude/projects with data)
cargo run

# API integration (requires Firefox logged into claude.ai)
# Should show: 📊 5h:X% 7d:X% at end of output
# Falls back silently to local data if API unavailable
```

**Testing checklist**:
- Context (🧠) updates with new messages
- Block cost (💰) matches billing cycles
- Burn rate (🔥) calculated correctly
- Performance <20ms average
- API metrics shown when Firefox logged in
- Fallback works when API unavailable

## Dependencies

- `reqwest` (rustls-tls) - HTTP for LiteLLM pricing fetch
- `curl` - libcurl bindings for claude.ai API (bypasses Cloudflare)
- `rusqlite` (bundled) - Firefox cookie extraction
- `chrono` - 5-hour block timestamps
- `serde`/`serde_json` - JSONL parsing
- `owo-colors` - Terminal colors
- `anyhow` - Error handling
- `fs2` - File locking for cache
- `libc` - UID lookup for XDG_RUNTIME_DIR

## Gotchas

- **Do NOT use `_var_name` to hide unused variables** (violates CLAUDE.md in parent)
- Version is ONLY in Cargo.toml, never edit PKGBUILD/Makefile versions
- CI checks formatting - must run `cargo fmt` before commit
- DO NOT push tags until CI passes on master
- API fallback is intentionally silent (only stderr for debugging)
- Firefox cookies.sqlite must use `immutable=1` mode (locked by Firefox)
