# Contributing

duit is at the scaffolding stage. Read [PLAN.md](PLAN.md) before starting work: it defines the
scope, the crate layout, and the order of the milestones.

## Setup

[Rust](https://rustup.rs/) and a C compiler are required to build duit. The toolchain version is
pinned in `rust-toolchain.toml`, and rustup installs it on first use.

## Testing

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

## Formatting

```shell
cargo fmt --all
taplo fmt
npx prettier@3.9.0 --write .
```

TOML files are formatted with [taplo](https://taplo.tamasfe.dev/).

## Linting

```shell
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

### Spell checking

```shell
typos
```

### Unused Rust dependencies

```shell
cargo shear
```

### Licenses and sources of dependencies

```shell
cargo deny --all-features check
```

## Git hooks

Hooks are optional. [prek](https://prek.j178.dev/) runs the formatting and spelling checks before a
commit and clippy before a push. The hooks call the tools above, so install those first:

```shell
prek install
```

To run every hook on all files:

```shell
prek run --all-files
prek run --all-files --hook-stage pre-push
```

## Crate structure

Rust does not allow circular dependencies between crates. The workspace is a flat list under
`crates/`:

- `duit`: the binary, option parsing, config files, and the main loop.
- `duit-model`: entry types, the `Sink` trait, and the in-memory tree.
- `duit-scan`: the filesystem scan.
- `duit-format`: JSON and binary import and export.
- `duit-tui`: the terminal UI.

PLAN.md maps each crate to the upstream ncdu source files that it ports.

## Releases

There is no release process yet. It is part of milestone M6.
