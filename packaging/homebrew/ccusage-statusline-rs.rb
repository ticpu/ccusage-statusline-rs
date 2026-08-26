# Checksums are placeholders on master and filled in by scripts/pin-packaging.sh
# on the release tag's own commit. A hash committed here is stale one release later.
class CcusageStatuslineRs < Formula
  desc "Claude Code statusline with usage, billing blocks and burn rate"
  homepage "https://github.com/ticpu/ccusage-statusline-rs"
  # Source build on Linux: the released binaries are static musl, and Homebrew
  # expects to link against its own glibc.
  url "https://github.com/ticpu/ccusage-statusline-rs/releases/download/v@VERSION@/ccusage-statusline-rs-@VERSION@.tar.xz"
  sha256 "@TARBALL_SHA256@"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/ticpu/ccusage-statusline-rs/releases/download/v@VERSION@/ccusage-statusline-rs-macos-aarch64"
      sha256 "@MACOS_AARCH64_SHA256@"
    end

    on_intel do
      url "https://github.com/ticpu/ccusage-statusline-rs/releases/download/v@VERSION@/ccusage-statusline-rs-macos-x86_64"
      sha256 "@MACOS_X86_64_SHA256@"
    end
  end

  on_linux do
    depends_on "rust" => :build
  end

  def install
    if OS.mac?
      bin.install Dir["ccusage-statusline-rs-macos-*"].first => "ccusage-statusline-rs"
    else
      system "cargo", "install", *std_cargo_args
    end
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ccusage-statusline-rs --version")
  end
end
