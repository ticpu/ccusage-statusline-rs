FROM docker.io/library/debian:trixie AS build

RUN --mount=type=cache,target=/var/cache/apt,id=apt-cache-trixie \
    --mount=type=cache,target=/var/lib/apt/lists,id=apt-lists-trixie \
    apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    gcc \
    libc6-dev \
    musl-tools \
    musl-dev \
    gcc-aarch64-linux-gnu \
    libc6-dev-arm64-cross \
    gcc-mingw-w64-x86-64

ENV CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup
ENV PATH="/usr/local/cargo/bin:${PATH}"

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal

RUN rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-gnu

# The Linux targets link static-pie against musl, so the binaries carry no libc
# dependency and the build needs no EOL base image to hold a glibc symbol floor.
# ring and mimalloc compile C. aarch64 has no musl cross gcc here, so the glibc one
# builds it with fortify disabled: _FORTIFY_SOURCE emits glibc-only __*_chk symbols
# that musl does not provide, and the static link fails on them.
ENV CC_x86_64_unknown_linux_musl=musl-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=yes" \
    CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc \
    CFLAGS_aarch64_unknown_linux_musl="-U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=0" \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=yes" \
    CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc \
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc

WORKDIR /app

# Cargo.lock is only committed on release commits, hence the glob
COPY Cargo.toml Cargo.lock* ./
COPY src ./src

# Serial by design: cargo already saturates the cores with -j, and concurrent
# invocations would block on the same build-directory lock.
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry \
    --mount=type=cache,target=/usr/local/cargo/git,id=cargo-git \
    --mount=type=cache,target=/app/target,id=cargo-target \
    set -eu; \
    for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-gnu; do \
        cargo build --release --target "$target"; \
    done; \
    mkdir /out; \
    cp target/x86_64-unknown-linux-musl/release/ccusage-statusline-rs /out/ccusage-statusline-rs-linux-x86_64; \
    cp target/aarch64-unknown-linux-musl/release/ccusage-statusline-rs /out/ccusage-statusline-rs-linux-aarch64; \
    cp target/x86_64-pc-windows-gnu/release/ccusage-statusline-rs.exe /out/ccusage-statusline-rs-windows-x86_64.exe

FROM scratch
COPY --from=build /out/ /
