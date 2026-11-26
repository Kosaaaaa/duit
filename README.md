# duit 📊🦀

[![CI](https://github.com/Kosaaaaa/duit/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/Kosaaaaa/duit/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.98.0-orange.svg?logo=rust)](Cargo.toml)
[![ncdu](https://img.shields.io/badge/ncdu-2.9.2-informational.svg)](https://dev.yorhel.nl/ncdu)
[![Status: M6 done](https://img.shields.io/badge/status-M6%20done-yellow.svg)](PLAN.md)

A disk usage analyzer with a terminal interface, written in Rust. duit aims to be a drop-in
replacement for [ncdu](https://dev.yorhel.nl/ncdu) 2: the same flags, the same keys, and the same
JSON and binary export formats.

## 🚧 Status

M6 is done: duit has every flag and key of ncdu 2.9.2, and `-t` scans with several threads. Scan
time and peak memory against ncdu are in [BENCHMARKS.md](BENCHMARKS.md), with every gap. `duit DIR`
scans a directory with the progress screen that `-0`, `-1`, or `-2` selects and opens a browser over
it, and `duit -f FILE` opens the same browser over a JSON export or reads a binary export straight
from the file. In the browser, `r` refreshes a directory, `d` deletes an entry, or runs the
`--delete-command` on it, after a confirmation, and `b` spawns a shell. `-r`, `-rr`, and the enable
and disable flags turn these actions off. `-o-` still writes ncdu's JSON export, `-c` compresses it,
and `-O` writes the binary export, so with `-f` duit also converts between formats. 0.1.0 is the
first release, and [CHANGELOG.md](CHANGELOG.md) lists what it contains. See [PLAN.md](PLAN.md) for
the architecture and the milestones.

## 📦 Installation

Install duit from [crates.io](https://crates.io/crates/duit):

```shell
cargo install --locked duit
```

Or install a prebuilt binary with the shell installer:

```shell
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Kosaaaaa/duit/releases/latest/download/duit-installer.sh | sh
```

The [GitHub Releases](https://github.com/Kosaaaaa/duit/releases) also have the archives the
installer downloads: static Linux binaries (musl) and macOS binaries, for x86_64 and aarch64, each
with the `duit.1` man page. There is no Windows build.

## 🤝 Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## 🙏 Acknowledgements

duit is a port of ncdu by Yorhel. The design of the scanner, the data model, and both export formats
come from ncdu.

## 📜 License

duit is licensed under the [MIT license](LICENSE).
