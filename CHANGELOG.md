# Changelog

## 0.1.2

Released on 2026-11-28.

The Release workflow publishes a Homebrew formula to the tap `Kosaaaaa/homebrew-tap`, so `brew install kosaaaaa/tap/duit` installs the prebuilt binary with its man page on macOS and Linux. The `duit` binary behaves as in 0.1.1.

## 0.1.1

Released on 2026-11-28.

The Release workflow publishes the five crates to crates.io with Trusted Publishing. The `duit` binary behaves as in 0.1.0.

## 0.1.0

Released on 2026-11-28.

The first release of duit, a drop-in replacement for ncdu 2.9.2 with the same flags, keys, config files, and export formats.

- Scan a directory with ncdu's scan options, including `-x`, `-L`, `-e`, exclude patterns, and the progress screens of `-0`, `-1`, and `-2`. `-t` scans with several threads.
- Write and read ncdu's JSON export with `-o` and `-f`, with zstd compression through `-c`.
- Write ncdu's binary export with `-O`, and browse it straight from the file with `-f`.
- Browse the tree with ncdu's keys, sorting, columns, info and help screens, colors, and display flags.
- Refresh a directory with `r`, delete an entry with `d` or the `--delete-command`, and spawn a shell with `b`. `-r` and `-rr` turn these off.
- Read `/etc/duit.conf` and the user config under `duit/`, or ncdu's config files when neither exists.
- Prebuilt binaries for Linux (static, x86_64 and aarch64) and macOS (x86_64 and aarch64), with the `duit.1` man page.
