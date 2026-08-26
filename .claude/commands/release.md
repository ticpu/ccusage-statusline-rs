Cut a release of ccusage-statusline-rs. Never start this process without explicit
instruction — "commit the fix" is not a release request.

Version lives ONLY in Cargo.toml; PKGBUILD and Makefile extract it. Never edit their
versions.

If $ARGUMENTS names a version or bump level (patch/minor/major), use it; otherwise ask
which bump is wanted before touching anything.

**Nothing generated reaches master.** `Cargo.lock` lives only on the tag's own commit, a
detached child of the green master commit, where `--locked` builds need it. Checksums
live nowhere in this repository: CI publishes them as a signed `SHA256SUMS` asset, and
`packaging/` carries `@PLACEHOLDER@` templates rendered into the tap at release time.

1. Preflight: working tree clean, on master, nothing unpushed
   (`git rev-list --count @{upstream}..HEAD`). If anything is unpushed, push it and wait
   for CI **on that commit** before touching the version — a red CI after the release
   commit leaves a `release:` commit on master that never shipped.

2. Edit `version` in Cargo.toml. Commit as `release: vX.Y.Z`, staging Cargo.toml alone.

3. Push the release commit and wait for CI on it:

```sh
git push
./scripts/watch-ci.sh
```

   `watch-ci.sh` pins the workflow and the commit SHA and passes `--exit-status`. Never
   select a run with `--branch ... --limit 1`: that reads whatever ran most recently on
   the branch, which need not be the commit being released.

4. Build the tag on a detached child of that green commit. Run these as **separate**
   commands, never chained with `&&`: if a chained command is rejected part-way the
   untried half is silently skipped, and the failure mode is committing the lockfile onto
   master because the detach never ran.

```sh
git checkout --detach
git symbolic-ref -q HEAD          # must FAIL — that is the confirmation the detach took
cargo generate-lockfile
git add -f Cargo.lock
git commit -m "build: pin Cargo.lock for vX.Y.Z"
git tag -as vX.Y.Z                # changelog in the tag message, see below
git push --tags
git switch master
```

   The tag is pushed once and never moved. `Cargo.lock` belongs to the tagged tree so
   PKGBUILD's `cargo build --locked` pins the set CI validated; it never lands on master,
   where it would conflict on every dependency bump.

5. Wait for the Release workflow, which leaves the release a **draft**:

```sh
./scripts/watch-ci.sh vX.Y.Z release.yml
```

6. `./sign-release.sh` — detach-signs every asset with the key from
   `git config user.signingkey`, uploads the `.asc` files, then publishes the draft.
   `--dry-run` signs and verifies without uploading. Nothing may be published unsigned:
   both AUR PKGBUILDs carry `validpgpkeys` and fail without the signatures, and with
   release immutability a published release's assets can no longer be added to.

7. Render the Homebrew formula for the tap. CI publishes a `SHA256SUMS` asset built from
   the assets themselves, so the checksums are read rather than recomputed, and they
   never enter a commit in this repository:

```sh
./scripts/pin-packaging.sh vX.Y.Z -o /path/to/homebrew-tap/Formula/ccusage-statusline-rs.rb
```

   Commit that in the tap repository. `packaging/homebrew/` here stays a placeholder
   template: a checksum on master is stale one release later.

8. Update **both** AUR packages:
   - `cd ~/.cache/paru/clone/ccusage-statusline-rs/ && ./update-pkg.sh 2>&1 | grep -v Compiling`
   - `cd ~/.cache/paru/clone/ccusage-statusline-rs-bin/ && ./update-pkg.sh`

   `update-pkg.sh` regenerates the PKGBUILD, so any hand edit to it (`depends`, for
   instance) must be re-applied **after** the script runs, followed by
   `makepkg --printsrcinfo > .SRCINFO`. AUR commits get no Co-Authored-By trailer.

9. Publish the Debian packages from the `-bin` clone: `./deploy-aptly.sh`. It reads the
   version from that PKGBUILD, so it runs after step 8. The script is never committed;
   it exists only in that working copy.

10. `cargo publish` from the tag if the crate version changed.

## Changelog

Goes in the tag message, not the commit. Written for someone not following development:
features, fixes, behaviour changes. No commit lists, no hashes, no co-author lines.

```
vX.Y.Z

Installation
- what changed about how it is installed

Fixes
- what was fixed, in user-visible terms
```
