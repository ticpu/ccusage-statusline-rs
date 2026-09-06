#!/bin/bash
# Update both AUR packages to a published release and push them.
#
# Usage: ./scripts/publish-aur.sh [vX.Y.Z]
#          default tag: v$(version from Cargo.toml)
#
# Each clone's own update-pkg.sh does the work: pkgctl version upgrade, .SRCINFO,
# a build, and a commit. It never pushes, so a release that stopped there left the
# AUR untouched while looking done. The version check below refuses to push a
# PKGBUILD that upgraded to something other than the tag being released, which is
# what happens when the GitHub release is not published yet.

set -euo pipefail

cd "$(dirname "$0")/.."

case "${1:-}" in
	-h|--help) sed -n '2,12p' "$0"; exit 0 ;;
	-*) echo "unknown option: $1" >&2; exit 1 ;;
esac

TAG="${1:-v$(grep -Po '^version = "\K[^"]+' Cargo.toml)}"
VERSION="${TAG#v}"
CLONES=(
	"$HOME/.cache/paru/clone/ccusage-statusline-rs"
	"$HOME/.cache/paru/clone/ccusage-statusline-rs-bin"
)

for clone in "${CLONES[@]}"; do
	if [[ ! -d "$clone" ]]; then
		echo "missing AUR clone: $clone" >&2
		echo "paru -G ${clone##*/} into ~/.cache/paru/clone first" >&2
		exit 1
	fi

	echo "== ${clone##*/}"
	(cd "$clone" && ./update-pkg.sh)

	pkgver=$(grep -Po '^pkgver=\K.*' "$clone/PKGBUILD")
	if [[ "$pkgver" != "$VERSION" ]]; then
		echo "${clone##*/}: PKGBUILD is at $pkgver, expected $VERSION" >&2
		exit 1
	fi

	git -C "$clone" push
done
