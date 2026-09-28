# Contributing to br8n

Thanks for helping. Bug reports, ideas and pull requests are all welcome.

## Reporting a bug

Open an [issue](https://github.com/Jagma/br8n/issues/new/choose) with your
`br8n --version`, your operating system, which agent you use, and what
`br8n status` says. `br8n status` prints file paths from your machine, so
remove anything you would rather not share before you paste it.

## Building and testing

You need a current stable Rust toolchain, a C++20 compiler (Xcode 16 on
macOS, GCC 13 or newer on Linux) and, on Linux, `libssl-dev` and
`pkg-config`. See [Build from source](docs/install.md#build-from-source).

```bash
cargo test                  # the full suite; no network and no Ollama needed
cargo clippy --all-targets  # must be free of warnings
cargo fmt --all
make dev                    # a faster build without backup and OCR
make test-dev               # the tests for that build
make ci                     # every CI job locally, plus the dashboard's browser tests
cargo build --release && target/release/br8n install   # try your build for real
```

The dashboard is a React app in `dashboard/`. Its built output in
`dashboard/dist/` is committed and embedded into the binary, so after changing
the dashboard run `npm ci && npm run build` in `dashboard/` and commit the
result.

[CLAUDE.md](CLAUDE.md) has more on the test layout and the performance guards.
It is written for coding agents, but it is just as useful for people.

## Pull requests

- Keep each pull request to one change, as a single commit. Squash review
  fixes into it.
- Write the commit subject as a plain sentence that says what changed, with no
  prefix like `fix:`, for example `search skips files that no longer exist`.
- Add or update a test for any behaviour you change, and run `make ci` (or at
  least `cargo test` and `cargo clippy`) before you open the PR.
- Prefer clear names and small functions to explanatory comments.

## Licence

br8n is licensed under the [AGPL-3.0](LICENSE). By opening a pull request you
agree that your contribution is licensed under the same terms.
