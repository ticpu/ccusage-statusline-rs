class CcusageStatuslineRs < Formula
  desc "Claude Code statusline with usage, billing blocks and burn rate"
  homepage "https://github.com/ticpu/ccusage-statusline-rs"
  # Source build everywhere except Apple Silicon, which has a prebuilt binary.
  url "https://github.com/ticpu/ccusage-statusline-rs/releases/download/v1.13.0/ccusage-statusline-rs-1.13.0.tar.xz"
  sha256 "864a5f3e58047b489745716f0e15c993a2cbde45b6b3f825d515da0c1115d368"
  license "MIT"

  depends_on "rust" => :build

  on_macos do
    on_arm do
      url "https://github.com/ticpu/ccusage-statusline-rs/releases/download/v1.13.0/ccusage-statusline-rs-macos-aarch64"
      sha256 "985b625ffe04fbff33f4ee8d4824c25a7471a4e057313df0eec469b48d0b973b"
    end
  end

  def install
    if OS.mac? && Hardware::CPU.arm?
      bin.install "ccusage-statusline-rs-macos-aarch64" => "ccusage-statusline-rs"
    else
      system "cargo", "install", *std_cargo_args
    end
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ccusage-statusline-rs --version")
  end
end
