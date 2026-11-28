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

On macOS, `crates/duit/build.rs` links the `duit` binary with `crates/duit/duit.order`, which puts
the functions of the streaming paths next to each other, so these paths keep fewer pages of code
resident. The file lists symbol names, which change with the code, the versions of the crates, and
the toolchain, and the linker skips the names that it does not find. Regenerate it with lldb before
a benchmark run or a release, and commit it:

```shell
make order
```

### Fuzzing

The `fuzz/` directory holds [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the
importers, which read untrusted files: `json_import` for JSON exports, plain or compressed with
zstd, and `bin_reader` for binary exports. It is a workspace of its own that needs a nightly
toolchain, and CI does not build it. To run a target for five minutes, seeded with the golden files:

```shell
rustup toolchain install nightly
cargo install --locked cargo-fuzz
mkdir -p fuzz/corpus/json_import
cargo +nightly fuzz run json_import fuzz/corpus/json_import crates/duit/tests/fixtures -- -max_total_time=300
```

The first directory collects the corpus. A crash is saved to `fuzz/artifacts/<target>/`, and
`cargo +nightly fuzz run <target> <file>` runs it again. Add the input to the tests of the importer
before fixing it.

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

The fuzz targets have a lockfile of their own, which the second command checks:

```shell
cargo deny --all-features check licenses sources bans
cargo deny --manifest-path fuzz/Cargo.toml --all-features check licenses sources bans
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

Hooks are optional. [prek](https://prek.j178.dev/) formats files and fixes unambiguous typos in
place before a commit, and runs clippy before a push. A hook that changes a file fails the commit,
so review and stage the changes, then commit again. The hooks call the tools above, so install those
first. `make tools` installs the versions that CI uses:

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

dist finds that entry by the version in its heading, such as `## 0.1.0`. Keep each paragraph and
list item of an entry on one line, because GitHub renders every line break in the release notes.

To release a version, for example `0.1.0`:

1. Set the version in `[workspace.package]` and in the internal crates in `[workspace.dependencies]`
   of the root `Cargo.toml`, and in the dependencies of `fuzz/Cargo.toml`, which CI does not build.
2. Replace `Not released yet.` in the CHANGELOG.md entry with `Released on YYYY-MM-DD.`, update the
   status in `README.md`, which the archives include, and merge the change into `master`.
3. Run the workflow with the default tag, `dry-run`, to build every artifact without publishing:
   `gh workflow run release.yml -f tag=dry-run`.
4. Run it with the tag on `master`: `gh workflow run release.yml -f tag=v0.1.0`. It creates the tag
   on the commit it ran on and the GitHub Release, and fails on any other branch or when the tag
   exists already.
5. Publish the crates from that tag by hand: `cargo publish --workspace`.

`.github/workflows/release.yml` is maintained by hand, so that it passes zizmor. A dist upgrade
changes `cargo-dist-version` in `dist-workspace.toml` together with `DIST_INSTALLER_URL` and
`DIST_INSTALLER_SHA256` in the workflow, which fails when the versions differ. After the upgrade,
compare the workflow with the output of `dist generate`.
