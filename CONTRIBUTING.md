# Contributing to duit 👋

Read [PLAN.md](PLAN.md) before starting work: it defines the scope, the crate layout, and the order
of the milestones.

## 🛠️ Setup

[Rust](https://rustup.rs/) and a C compiler are required to build duit. The toolchain version is
pinned in `rust-toolchain.toml`, and rustup installs it on first use.

Run `make help` to list the shortcuts for the commands in this guide. `make check` runs the CI
checks except zizmor and the cargo publish dry run, fixing TOML, Markdown, YAML, and JSON formatting
in place, then the tests the way CI does.

## 🧪 Testing

For running tests, we recommend [nextest](https://nexte.st/).

To run all tests:

```shell
cargo nextest run
```

To run a specific test by name:

```shell
cargo nextest run -E 'test(test_name)'
```

### Snapshot testing

duit uses [insta](https://insta.rs/) for snapshot testing. It's recommended (but not necessary) to
use `cargo-insta` for a better snapshot review experience. See the
[installation guide](https://insta.rs/docs/cli/) for more information.

To run all tests and accept snapshot changes:

```shell
cargo insta test --accept --test-runner nextest
```

### Testing against ncdu

Tests that run the real ncdu are behind the `test-ncdu` feature. They need ncdu 2.9.2 on `PATH`:

```shell
cargo nextest run --features test-ncdu
```

The golden files in `crates/duit/tests/fixtures/` are exports that ncdu 2.9.2 wrote. The tests read
them without ncdu. To write them again, with ncdu 2.9.2 on `PATH`:

```shell
crates/duit/tests/fixtures/generate.sh
```

### Benchmarks

`scripts/benchmark.py` compares the scan time and the peak memory of a release build of duit with
those of ncdu 2.9.2, on a generated tree of about one million entries. It needs Python 3,
[hyperfine](https://github.com/sharkdp/hyperfine), and ncdu 2.9.2 on `PATH` (or `--ncdu PATH`), and
takes about 20 minutes:

```shell
python3 scripts/benchmark.py
```

The results go to `target/benchmark/results.md`. [BENCHMARKS.md](BENCHMARKS.md) records the last
run.

## 🎨 Formatting

```shell
cargo fmt --all
taplo fmt
npx prettier@3.9.0 --write .
```

TOML files are formatted with [taplo](https://taplo.tamasfe.dev/).

## 🔍 Linting

```shell
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

### Spell checking

```shell
typos
```

### Unused Rust dependencies

```shell
cargo shear --deny-warnings
```

### Licenses and sources of dependencies

```shell
cargo deny --all-features check licenses sources bans
```

### Man page

The man page `crates/duit/duit.1` is linted with [mandoc](https://mandoc.bsd.lv/), which macOS ships
and Linux distributions package as `mandoc`:

```shell
mandoc -T lint -W warning crates/duit/duit.1
```

`-W warning` leaves out style messages, such as a referenced manual that is not installed, which
depend on the manuals of the machine.

## 🪝 Git hooks

Hooks are optional. [prek](https://prek.j178.dev/) runs the formatting and spelling checks before a
commit and clippy before a push. The hooks call the tools above, so install those first.
`make tools` installs the versions that CI uses:

```shell
make tools
prek install
```

To run every hook on all files:

```shell
prek run --all-files
prek run --all-files --hook-stage pre-push
```

## 🔀 Pull requests

`master` only accepts changes through pull requests, and every pull request is squash-merged. The
title becomes the commit subject and the description becomes the commit body, so write both for
someone reading `git log`:

- Write the title as an imperative sentence in sentence case, with no trailing period and no type
  prefix such as `feat:`. For example: `Port the --exclude option`.
- Fill in the Summary (what changed and why) and the Test Plan sections of the template, and delete
  its HTML comments.
- Keep each pull request to one change, since it lands as one commit.

The `all required jobs passed` check must succeed before a pull request can be merged.

Branch names are free-form. Prefer short kebab-case names, such as `exclude-option`.

## 📦 Crate structure

Rust does not allow circular dependencies between crates. The workspace is a flat list under
`crates/`:

- `duit`: the binary, option parsing, config files, and the main loop.
- `duit-model`: entry types, the `Sink` trait, and the in-memory tree.
- `duit-scan`: the filesystem scan.
- `duit-format`: JSON and binary import and export.
- `duit-tui`: the terminal UI.

PLAN.md maps each crate to the upstream ncdu source files that it ports.

## 🚀 Releases

Releases are built by [dist](https://axodotdev.github.io/cargo-dist/), configured in
`dist-workspace.toml`. The `Release` workflow builds static Linux binaries (musl) and macOS
binaries, packs each with `LICENSE`, `README.md`, and `duit.1`, and attaches them to a GitHub
Release together with a shell installer. Its release notes are the entry for the version in
[CHANGELOG.md](CHANGELOG.md). The workflow never publishes to crates.io.

To release a version, for example `0.1.0`:

1. Set the version in `[workspace.package]` and in the internal crates in `[workspace.dependencies]`
   of the root `Cargo.toml`, and update the snapshots that show it.
2. Replace `Not released yet.` in the CHANGELOG.md entry with `Released on YYYY-MM-DD.`, update the
   status in `README.md`, which the archives include, and merge the change into `master`.
3. Run the workflow with the default tag, `dry-run`, to build every artifact without publishing:
   `gh workflow run release.yml -f tag=dry-run`.
4. Run it with the tag: `gh workflow run release.yml -f tag=v0.1.0`. It creates the tag on the
   commit it ran on and the GitHub Release.
5. Publish the crates from that tag by hand: `cargo publish --workspace`.

`.github/workflows/release.yml` is maintained by hand, so that it passes zizmor. After a dist
upgrade, compare it with the output of `dist generate`.
