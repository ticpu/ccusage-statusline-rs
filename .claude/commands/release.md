Cut a release of ccusage-statusline-rs. Never start this process without explicit instruction — "commit the fix" is not a release request.

Version lives ONLY in Cargo.toml; PKGBUILD and Makefile auto-extract it. Never edit their versions.

If $ARGUMENTS names a version or bump level (patch/minor/major), use it; otherwise ask which bump is wanted before touching anything.

1. Preflight: working tree clean, on master, and CI green **for the commits being released** — not merely for whatever master last ran. Check both:
   - `git rev-list --count @{upstream}..HEAD` — unpushed commits.
   - `gh run list --branch master --limit 1` — and confirm the run's commit is HEAD.
   If anything is unpushed, push it and wait for CI to go green **before** touching the version. A failing CI then costs nothing; a failing CI after the release commit leaves a `release:` commit on master that never shipped, which has to be unwound by hand.
2. Edit `version` in Cargo.toml.
3. `cargo generate-lockfile`, then `git add -f Cargo.lock` — the release tarball is `git archive HEAD` at the tag, so the lockfile must be committed for PKGBUILD's `cargo build --locked` to pin anything.
4. Commit as `release: vX.Y.Z`, staging Cargo.toml and Cargo.lock explicitly.
5. `git push`, then WAIT for CI to pass on master (`gh run watch`).
6. `git tag -as vX.Y.Z` — changelog goes in the tag message: features, fixes, API changes for someone not following development. No commit lists or hashes.
7. `git push --tags`, then WAIT for the Release workflow to complete successfully (`gh run watch`).
8. Update the AUR package: `cd ~/.cache/paru/clone/ccusage-statusline-rs/ && ./update-pkg.sh 2>&1 | grep -v Compiling` — it should print the new version; troubleshoot only if it fails. AUR commits get no Co-Authored-By trailer.
