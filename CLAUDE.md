# CLAUDE.md

Development guide for Claude Code when working on ccusage-statusline-rs.

## Project Overview

Ultra-fast Rust implementation of Claude Code usage statusline. Provides 8ms average performance (15x faster than warm Node.js) for real-time usage tracking with accurate cost calculation.

## Repository Structure

```
.
├── src/
│   ├── main.rs       # Entry point and statusline generation
│   ├── types.rs      # All struct definitions and implementations
│   ├── pricing.rs    # LiteLLM pricing fetcher with caching
│   ├── blocks.rs     # 5-hour billing block logic and deduplication
│   ├── cache.rs      # Semaphore-based output caching
│   └── format.rs     # Output formatting functions
├── .github/workflows/
│   ├── ci.yml        # CI workflow for master branch
│   └── release.yml   # Release workflow for tags
├── Cargo.toml        # Single source of truth for version
├── PKGBUILD         # Arch Linux package (auto-extracts version)
└── Makefile         # Build automation (auto-extracts version)
```

## Version Management

**IMPORTANT**: Version is managed in `Cargo.toml` only. Both `PKGBUILD` and `Makefile` automatically extract the version using:

```bash
grep -Po '^version = "\K[^"]+' Cargo.toml
```

To bump version:
1. Edit version in `Cargo.toml` only
2. **CRITICAL**: Run `cargo fmt` and `cargo clippy --fix --allow-dirty` before committing (CI will fail otherwise)
3. Commit changes
4. Push commit: `git push`
5. **CRITICAL**: Wait for CI to pass on master before creating tag
6. Once CI passes, create tag: `git tag -as vX.Y.Z -m "Release vX.Y.Z"`
7. Push tag: `git push --tags`

**DO NOT push tags until CI passes on master. Failed builds will block releases.**

## Development Commands

### Code Quality

**CRITICAL: Always run `cargo fmt` before committing. CI checks formatting and will fail if code is not formatted.**

```bash
# Format code - MUST run before every commit
cargo fmt

# Run clippy with auto-fix
cargo clippy --fix --allow-dirty --message-format=short

# Type check (faster than build)
cargo check --message-format=short

# Run tests
cargo test --message-format=short

# Build release binary
cargo build --release
```

**Pre-commit checklist**:
- [ ] `cargo fmt` - Code is formatted
- [ ] `cargo clippy --fix --allow-dirty` - No clippy warnings
- [ ] `cargo test` - All tests pass
- [ ] `cargo build --release` - Release build succeeds

### Testing

```bash
# Test with sample data from ccusage repo
cat /path/to/ccusage/apps/ccusage/test/statusline-test.json | cargo run

# Compare Rust vs Node.js output
cat /path/to/ccusage/apps/ccusage/test/statusline-test.json | ./target/release/ccusage-statusline-rs
cat /path/to/ccusage/apps/ccusage/test/statusline-test.json | node /path/to/ccusage/apps/ccusage/dist/index.js statusline --visual-burn-rate emoji

# Run performance benchmark (requires ccusage repo)
bash benchmark.sh

# Build and install package locally
make package

# Clean build artifacts
make clean
```

**Testing Checklist**:
- [ ] Outputs match between Rust and Node.js implementations
- [ ] Context window (🧠) updates with new messages
- [ ] Block costs (💰) match billing cycles
- [ ] Burn rate (🔥) calculated correctly
- [ ] Performance is <20ms average

## CI/CD Workflows

### CI Workflow (`ci.yml`)

Triggers on:
- Push to `master` branch
- Pull requests to `master`

Jobs:
1. **x86_64 build**: Format check, clippy, build, tests
2. **aarch64 build**: Cross-compilation build only

### Release Workflow (`release.yml`)

Triggers on:
- Push tags matching `v*` pattern

Jobs:
1. **Create Release**: Generate source tarball and create GitHub release
2. **Build Binaries**: Build x86_64 and aarch64 Linux binaries in parallel

**Key Implementation Details**:
- Uses `dtolnay/rust-toolchain@stable` (not deprecated actions-rs)
- Uses `rustls-tls` instead of `native-tls` for easier cross-compilation
- Sets `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc`
- Sets `PKG_CONFIG_ALLOW_CROSS=1` for aarch64 builds

## Release Process

1. Ensure all tests pass: `cargo test`
2. Update version in `Cargo.toml`
3. Run formatting: `cargo fmt`
4. Commit: `git commit -m "chore: bump version to X.Y.Z"`
5. Tag release: `git tag -as vX.Y.Z -m "Release vX.Y.Z"`
6. **Push commits AND tags**: `git push && git push --tags`
7. GitHub Actions automatically:
   - Creates release with generated notes
   - Builds and uploads source tarball
   - Builds and uploads x86_64 and aarch64 binaries

## Dependencies

### Runtime (all in devDependencies for bundling)
- `reqwest` with `rustls-tls` - HTTP client for LiteLLM pricing
- `chrono` - Date/time handling for 5-hour blocks
- `serde`/`serde_json` - JSON parsing for JSONL files
- `owo-colors` - Terminal color output
- `anyhow` - Error handling
- `fs2` - File locking for cache
- `libc` - UID lookup for XDG_RUNTIME_DIR

### Build
- `cargo` - Rust build system
- For aarch64: `gcc-aarch64-linux-gnu`, `pkg-config`

## Code Style

- Follow Rust standard formatting (`cargo fmt`)
- All clippy warnings must be fixed
- Prefer early returns over nested conditionals
- Use Result types for error handling
- Keep functions focused and small

## Architecture Notes

**Performance Optimizations**:
- XDG_RUNTIME_DIR caching with 30-second TTL
- LiteLLM pricing cached for 24 hours
- Semaphore-based output caching with file locking
- Deduplication using `messageId:requestId` hash

**5-Hour Billing Blocks**:
- Matches Claude's billing cycles exactly
- Floors timestamps to beginning of hour
- Tracks active blocks for burn rate calculation
- Supports tiered pricing at 200k token threshold

**LiteLLM Integration**:
- Fetches pricing from GitHub daily
- Supports tiered pricing for Claude models
- Falls back to hardcoded prices if unavailable
- Handles cache token pricing (creation + read)

## Troubleshooting

**Build failures on aarch64**:
- Ensure `rustls-tls` is used (not `native-tls`)
- Check `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER` is set
- Verify `PKG_CONFIG_ALLOW_CROSS=1` is set

**CI formatting failures**:
- Run `cargo fmt` before commit
- Check `.editorconfig` for line endings

**Version mismatch in builds**:
- Version is auto-extracted from `Cargo.toml`
- Never manually edit version in `PKGBUILD` or `Makefile`

## Testing Checklist

Before creating a release:
- [ ] `cargo fmt` - Code formatted
- [ ] `cargo clippy --fix --allow-dirty` - No warnings
- [ ] `cargo test` - All tests pass
- [ ] `cargo build --release` - Release build succeeds
- [ ] `make package` - Package builds successfully
- [ ] CI passes on master branch
- [ ] Version updated in `Cargo.toml` only