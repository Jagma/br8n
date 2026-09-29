# Cutting a release


This is for maintainers.

There is no tag step. Bump `version` in `Cargo.toml`, add a `## <version>`
section to `CHANGELOG.md`, and merge to `main`. The `release` workflow builds
every target, runs `scripts/release-smoke.sh` against each packaged tarball,
and publishes `v<version>` with all of them and `install.sh` at once. A push
to `main` whose version is already released does nothing.

A release must carry all three targets; the workflow refuses to publish
one that is missing any of them.

The macOS binary links OpenSSL statically and needs nothing from Homebrew. It
is signed ad hoc, not notarized, because there is no Apple Developer account.
`install.sh`, `gh release download` and `br8n update` fetch it with tools
that set no quarantine attribute, so it runs without a warning. A tarball
saved through a browser is quarantined; see Option C, step 4.

The workflow takes roughly ten to twenty minutes between the merge and the
release being published. `br8n update` in that window reports the previous
release as the latest.

Once the release is published, bring the Homebrew formula up to date in a
clone of [Jagma/homebrew-tap](https://github.com/Jagma/homebrew-tap), then
commit and push it there:

```bash
scripts/homebrew-formula.sh <version> > ../homebrew-tap/Formula/br8n.rb
```

The script reads each build's checksum from the published release, so it
fails until the release has both the macOS and the Linux build.

To withdraw a bad release: `gh release delete v<version> --cleanup-tag --yes`,
then push to `main` again. Without `--cleanup-tag` the tag survives, and the
gate refuses that version on every push until the tag is gone.

## Building a release by hand

When the workflow cannot run, `scripts/build-release.sh [<target>]` builds,
packages and smoke-tests one target on the current machine, writing
`dist/br8n-<target>.tar.gz` and its `.sha256`. It links OpenSSL statically
on macOS without modifying Homebrew, remaps build paths so the binary does
not contain the builder's home directory, and packs the archive with owner
0/0 so its headers do not carry the builder's user name. Upload the files to a draft
release together with `scripts/install.sh`, then publish it.
