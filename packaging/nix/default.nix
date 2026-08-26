# A reference derivation: point `version` at a release tag and replace both
# lib.fakeHash values with what the first build prints. They are derived from the
# git tree and Cargo.lock rather than from the release binaries, so unlike the
# Homebrew formula there is nothing for a release to pin them to.
{ lib, rustPlatform, fetchFromGitHub }:

rustPlatform.buildRustPackage rec {
  pname = "ccusage-statusline-rs";
  version = "1.14.0";

  src = fetchFromGitHub {
    owner = "ticpu";
    repo = "ccusage-statusline-rs";
    rev = "v${version}";
    hash = lib.fakeHash;
  };

  cargoHash = lib.fakeHash;

  # The statusline reads ~/.claude and calls the usage endpoint, and the render
  # budget test asserts wall-clock timing, so neither survives the sandbox.
  doCheck = false;

  meta = with lib; {
    description = "Claude Code statusline with usage, billing blocks and burn rate";
    homepage = "https://github.com/ticpu/ccusage-statusline-rs";
    license = licenses.mit;
    maintainers = [ ];
    mainProgram = "ccusage-statusline-rs";
    platforms = platforms.unix ++ platforms.windows;
  };
}
