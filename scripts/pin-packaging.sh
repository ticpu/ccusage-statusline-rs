#!/bin/bash
# Render the Homebrew formula for a published release, with real checksums.
#
# Usage: ./scripts/pin-packaging.sh [vX.Y.Z] [-o OUTPUT]
#          default tag:    v$(version from Cargo.toml)
#          default output: stdout
#
# The checksums come from the release's own SHA256SUMS asset, which CI generates
# from the assets it just built. Nothing is re-hashed here, so this cannot
# disagree with what was published.
#
# The rendered formula is not committed to this repository: it belongs in the tap
# (ticpu/homebrew-tap), and a checksum on master is stale one release later.
# packaging/homebrew/ccusage-statusline-rs.rb is the template it renders.

set -euo pipefail

cd "$(dirname "$0")/.."

TAG=""
OUTPUT=""
while (( $# )); do
	case "$1" in
		-o|--output) OUTPUT="$2"; shift ;;
		-h|--help) sed -n '2,15p' "$0"; exit 0 ;;
		-*) echo "unknown option: $1" >&2; exit 1 ;;
		*) TAG="$1" ;;
	esac
	shift
done

TAG="${TAG:-v$(grep -Po '^version = "\K[^"]+' Cargo.toml)}"
VERSION="${TAG#v}"
TEMPLATE=packaging/homebrew/ccusage-statusline-rs.rb

WORKDIR="$(mktemp -d scratch/pin-XXXXXX)"
trap 'rm -rf "$WORKDIR"' EXIT

gh release download "$TAG" --dir "$WORKDIR" --pattern SHA256SUMS

# A missing entry means the asset set changed and the template needs updating;
# an empty value silently produces a formula that fails for users.
sha_of() {
	local name=$1 value
	value="$(awk -v n="$name" '$2 == n { print $1 }' "$WORKDIR/SHA256SUMS")"
	if [ -z "$value" ]; then
		echo "$name is absent from $TAG's SHA256SUMS" >&2
		exit 1
	fi
	printf '%s' "$value"
}

# Resolved before substitution: a sha_of failure inside sed's argument list would
# only kill that command substitution, and the script would exit 0 having written
# a formula with an empty checksum.
TARBALL_SHA="$(sha_of "ccusage-statusline-rs-$VERSION.tar.xz")"
MACOS_ARM_SHA="$(sha_of ccusage-statusline-rs-macos-aarch64)"
MACOS_INTEL_SHA="$(sha_of ccusage-statusline-rs-macos-x86_64)"

rendered="$(
	sed -e "s|@VERSION@|$VERSION|g" \
		-e "s|@TARBALL_SHA256@|$TARBALL_SHA|" \
		-e "s|@MACOS_AARCH64_SHA256@|$MACOS_ARM_SHA|" \
		-e "s|@MACOS_X86_64_SHA256@|$MACOS_INTEL_SHA|" \
		"$TEMPLATE"
)"

# A surviving placeholder means the template grew one this script does not know
# about, and it would reach the tap as a literal @NAME@.
if leftovers="$(grep -n '@[A-Z_]\+@' <<<"$rendered")"; then
	echo "unsubstituted placeholders remain:" >&2
	echo "$leftovers" >&2
	exit 1
fi

# The first two lines of the template explain the placeholders; they are wrong
# once rendered.
rendered="$(sed '1,2d' <<<"$rendered")"

if [ -n "$OUTPUT" ]; then
	printf '%s\n' "$rendered" > "$OUTPUT"
	echo "wrote $OUTPUT for $TAG" >&2
else
	printf '%s\n' "$rendered"
fi
