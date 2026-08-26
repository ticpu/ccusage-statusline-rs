{ lib, rustPlatform, fetchFromGitHub }:

rustPlatform.buildRustPackage rec {
  pname = "ccusage-statusline-rs";
  version = "1.13.0";

  src = fetchFromGitHub {
    owner = "ticpu";
    repo = "ccusage-statusline-rs";
    rev = "v${version}";
    hash = "sha256-iDI059tE2YPUG/3sE+y7oihvDek0RqcWWty4mcL0vPY=";
  };

  cargoHash = "sha256-Pjc+Ce3oRfzAJzKVaNjltykRIROT4AW8SwDmtIf6VNo=";

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
