# duit

A disk usage analyzer with a terminal interface, written in Rust. duit aims to be a drop-in
replacement for [ncdu](https://dev.yorhel.nl/ncdu) 2.9.2: the same flags, the same keys, and the
same JSON and binary export formats. It runs on Linux and macOS.

```shell
cargo install --locked duit
duit DIR
```

The manual is in [`duit.1`](https://github.com/Kosaaaaa/duit/blob/master/crates/duit/duit.1). See
the [repository](https://github.com/Kosaaaaa/duit) for more information.

This crate is the entry point to the duit command-line interface. The Rust API exposed here is not
considered public interface.

## Acknowledgements

duit is a port of ncdu by Yorhel. The design of the scanner, the data model, and both export formats
come from ncdu.

## License

duit is licensed under the [MIT license](https://github.com/Kosaaaaa/duit/blob/master/LICENSE).
