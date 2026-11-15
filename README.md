# duit 📊🦀

[![CI](https://github.com/Kosaaaaa/duit/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/Kosaaaaa/duit/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.98.0-orange.svg?logo=rust)](Cargo.toml)
[![ncdu](https://img.shields.io/badge/ncdu-2.9.2-informational.svg)](https://dev.yorhel.nl/ncdu)
[![Status: M1 done](https://img.shields.io/badge/status-M1%20done-yellow.svg)](PLAN.md)

A disk usage analyzer with a terminal interface, written in Rust. duit aims to be a drop-in
replacement for [ncdu](https://dev.yorhel.nl/ncdu) 2: the same flags, the same keys, and the same
JSON and binary export formats.

## 🚧 Status

M1 is done: `duit -o- DIR` scans a directory and writes ncdu's JSON export. There is no UI and no
release yet. See [PLAN.md](PLAN.md) for the architecture and the milestones.

## 🤝 Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## 🙏 Acknowledgements

duit is a port of ncdu by Yorhel. The design of the scanner, the data model, and both export formats
come from ncdu.

## 📜 License

duit is licensed under the [MIT license](LICENSE).
