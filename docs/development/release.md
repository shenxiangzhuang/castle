# Release workflow

The manifests, `.github/workflows/release.yml`, and `scripts/release-matrix.py` are the source of truth.

1. Create `release/<version>` from the default branch. Update the workspace version.
   Stable releases use a minor bump with patch `0`;
   prereleases use Cargo semver such as `0.2.0-alpha.1`.
2. Open a pull request and wait for every CI check on its current head commit to pass. When the
   user requests a release, agents and automation may merge the release pull request and publish
   it without a separate manual-merge confirmation. Guard the merge against head changes.
3. After confirming the pull request was merged, publish a GitHub Release from the merged commit
   with tag `v<version>`. Mark alpha and beta releases as prereleases.

Publishing the GitHub Release triggers the workflow that builds native desktop installers,
uploads GitHub Release assets, and updates the R2 feeds. All three application crates are internal
(`publish = false`). Manual dispatch rebuilds desktop assets for an existing release tag.


## Castle identity and update hosting

The product and Velopack package ID are `Castle`; the bundle ID is `dev.castle.desktop`.
macOS ships `Castle.app` with `Contents/MacOS/castle`. Linux and Windows run
`castle-desktop` / `castle-desktop.exe`; installer filenames start with `castle-desktop-`.
macOS builds separate ARM64 and Intel packages. The workflow checks both the executable and
`UpdateMac` architecture, then verifies the bundle signature.

Before the first Castle release, rename the GitHub repository to `shenxiangzhuang/castle`,
configure `updates.castle.mathewshen.me` for the R2 bucket, and set the repository variable
`R2_PUBLIC_BASE_URL` to `https://updates.castle.mathewshen.me` (without a trailing slash).
Existing R2 endpoint/bucket variables and upload credentials still select the storage destination.
All new uploads and cleanup use `castle/<channel>/<target>/`; the client uses the same URL path.
This namespace prevents overwriting previous product feeds even when sharing a bucket.

Castle has a new installation identity: install it once from the new GitHub Release; seamless
cross-product updates are not implemented. Quit Kcastle before starting Castle for the first time.
The harness automatically renames `~/.kcastle` to `~/.castle` when the latter does not exist,
including SQLite sidecars and all project data. Both directories existing means no merge or overwrite.
`CASTLE_DATA_DIR` explicitly selects a directory and bypasses automatic migration.
See [storage](../architecture/app-storage.md) for the migration boundary.

Keep existing Kcastle feeds, installers, and the frozen Universal bridge at their original URLs.
Do not rerun old tags as Castle releases: manual dispatch checks out the requested tag's workflow
helpers and source, which retain their historical identity.

## Verification

Run `python3 scripts/release-matrix.py --self-test`,
`cargo test -p desktop --locked updater::tests`, and `scripts/package-macos-app debug`.
These check naming consistency, native feed routing, version selection, and local bundling.
Actual DNS/R2 availability, Windows/Linux installers, and native update replacement require
release-environment validation; local checks do not publish or configure hosting.
