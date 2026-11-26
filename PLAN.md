# Plan

duit is a Rust rewrite of [ncdu](https://dev.yorhel.nl/ncdu) 2. The reference is ncdu 2.9.2: the
source at <https://code.blicky.net/yorhel/ncdu>, tag `v2.9.2`, about 6,800 lines of Zig in 16 files.
This document records what gets built, how it is split, and in which order.

## Goals

1. Drop-in compatibility with ncdu 2.9.2: flags, config file syntax, key bindings, JSON export, and
   binary export.
2. Parity first. No new features before milestone M4 is complete.
3. Scan time and peak memory at or below ncdu's, measured on the same tree.
4. One static binary without a system ncurses.

## Non-goals

- Windows. The model depends on POSIX `stat` fields (blocks, device, inode, link count).
- New flags, new file formats, or an async runtime.
- Compatibility with ncdu 1.x beyond what 2.9.2 itself accepts.

## Compatibility contract

| Surface       | Contract                                                                             |
| ------------- | ------------------------------------------------------------------------------------ |
| CLI flags     | Every flag in `ncdu.1` is accepted with the same meaning and the same default.       |
| Config file   | Same syntax: one option per line, `#` comments, `@` prefix to ignore a failing line. |
| Key bindings  | Same keys, same actions.                                                             |
| JSON export   | ncdu imports duit's output, and duit imports ncdu's. Item data is equal.             |
| Binary export | ncdu reads duit's files, and duit reads ncdu's. Decoded items are equal.             |
| Terminal UI   | Same screens, columns, and information. Not cell-for-cell identical.                 |
| Exit status   | Same success and failure conditions.                                                 |

Not covered: the wording of error messages and of `--help`, the metadata block of exports
(`progname`, `progver`, `timestamp`), and the byte layout of compressed blocks.

Where the upstream code and its man page disagree, the code decides.

## Architecture

Every producer pushes entries into a sink, and every consumer is a sink. This is the upstream design
(`sink.zig`), and it is what makes all combinations of `-f`, `-o`, and `-O` work without special
cases.

```text
 producers                 seam                 consumers

 filesystem scan  ---+                    +---> in-memory tree ---> browser
 JSON import      ---+---> Sink trait ----+---> JSON export
 binary import    ---+                    +---> binary export
 in-memory tree   ---+
```

The browser reads from one of two sources: the in-memory tree, or a binary export that is decoded
lazily block by block. A binary export of a huge tree can therefore be browsed without loading it.

### Crates

| Crate         | Ports from upstream                                                      | Depends on                  |
| ------------- | ------------------------------------------------------------------------ | --------------------------- |
| `duit-model`  | `model.zig`, `sink.zig`, `mem_sink.zig`, `mem_src.zig`                   | none                        |
| `duit-scan`   | `scan.zig`, `exclude.zig`                                                | `duit-model`                |
| `duit-format` | `json_export.zig`, `json_import.zig`, `bin_export.zig`, `bin_reader.zig` | `duit-model`                |
| `duit-tui`    | `ui.zig`, `browser.zig`, `delete.zig`, `sink.zig`                        | `duit-model`, `duit-format` |
| `duit`        | `main.zig`                                                               | all of the above            |

- `duit-model`: entry types, the `Sink` trait, the in-memory tree, hard link accounting, and size
  aggregation.
- `duit-scan`: the directory walk, `stat`, exclude patterns, kernfs detection, `CACHEDIR.TAG`, `-x`,
  and `-L`.
- `duit-format`: JSON and binary import and export, with zstd.
- `duit-tui`: scan progress, browser, info, help, delete, and shell screens.
- `duit`: option parsing, config files, mode dispatch, and the `duit` binary.

The crates are split on the seams that exist upstream. A new crate needs a second consumer.

The `duit` crate follows the uv layout: a library plus `src/bin/duit.rs`. The binary is added in M1.

## Decisions

- **Terminal: `ratatui` with `crossterm`, not ncurses.** No C dependency, static builds, and
  `TestBackend` makes the UI snapshot-testable. Cost: the ncdu look is rebuilt by hand.
- **Option parsing: a port of ncdu's own parser, not `lexopt` or `clap`.** ncdu parses arguments in
  the style of `getopt_long` with its own `Args`, and feeds config file lines through it one line at
  a time, with per-line error suppression, paired on and off flags where the last one wins, and a
  counted `-r`. `lexopt` differs at the edges: it reads a lone `-` as a positional argument, and it
  decodes option names as UTF-8. The port works on bytes and matches ncdu's messages exactly.
- **Program name: the first argument is skipped whole, unlike ncdu.** At `main.zig:510`, ncdu 2.9.2
  drops the program name with `args.next()`. If the name starts with `-`, that call returns only its
  first short option, and the rest of the name is parsed as more short options. Started as `-ncdu`,
  the way a login shell names its program, ncdu sets `-c` and then stops with
  `Unrecognized option '-d'.` while duit skips the first argument with `skip(1)` and runs the scan.
- **System calls: `rustix`.** Safe `openat`, `statat`, `fstatfs`, and directory iteration. The scan
  needs no `unsafe`.
- **Exclude patterns: `libc::fnmatch` behind one wrapper.** ncdu calls libc `fnmatch`. Any
  reimplementation risks different matches.
- **JSON: a hand-written streaming reader and writer.** ncdu writes non-UTF-8 name bytes raw, so
  exports are not always valid JSON and `serde_json` rejects them. Files can be gigabytes.
- **Binary format: a hand-written CBOR subset and the `zstd` crate.** The format uses a small fixed
  subset of CBOR. `zstd` binds libzstd, which is what ncdu links.
- **Tree memory: index-based arenas in safe Rust, with at most 2^32 - 1 entries, unlike ncdu.**
  Entries and names live in `Vec`-backed arenas and refer to each other by id. Budget: peak memory
  at most 1.25 times ncdu's. Each id is a `u32` with a `NonZero` niche, so an `Option` of an id
  takes 4 bytes too, and a name ends where the name of the next entry starts, so an entry stores no
  name length. An entry takes 40 bytes, a directory 40 more, and a hard link 32 more. That meets the
  budget without `unsafe`, as measured in [BENCHMARKS.md](BENCHMARKS.md), so there is no packed
  layout. ncdu has no limit on the number of entries. A scan or an import that would add more stops
  with `Maximum number of entries exceeded.` The offset of a name is a `usize`, so the names have no
  limit of their own.
- **Threads: `std::thread` with one writer, unlike ncdu.** At `scan.zig:271-293`, each ncdu thread
  reads its directory entry by entry and reports every entry to the sink itself, so the sinks lock
  what threads share. With `-t` above 1, duit threads take directories from a mutex and condvar
  queue, last in first out as in ncdu, read and `stat` each one, and send its entries in parts of at
  most 64 over a channel of 16 parts to the calling thread, so that a large directory does not wait
  whole in memory. That thread alone owns the sink, reports the entries of each directory in the
  order of `readdir`, and finishes a directory once its entries and its subdirectories are. A
  directory with subdirectories in a part before its last keeps a copy of its descriptor for them to
  be opened in, as its thread reads on. The sinks stay single-threaded, and the tree is ncdu's, so
  `-o` writes the same export. No `rayon`, no async. The default stays one thread, as in ncdu, which
  scans depth-first without the queue. While no directory arrives, the calling thread lets the
  progress handle an event every 100 ms, where ncdu's first thread handles one for each entry it
  reads. The progress shows a line for each thread, as ncdu does: the directory from when the thread
  takes it until the calling thread takes its entries, which stands for ncdu reporting them, and
  `(waiting)` while it waits for one. No thread reads yet when the root is created, where ncdu's
  first thread shows the root at its first event. A `-o` export with more than one thread is written
  from the tree after the scan, as in ncdu, and a root that cannot be read ends it as with one
  thread, where ncdu 2.9.2 crashes: `done` replays a tree whose root was never created at
  `sink.zig:265-268`. A `-O` export with more than one thread is written as the calling thread
  reports the scan, into one buffer as with one thread, where each ncdu thread fills a buffer of its
  own (`bin_export.zig:88-149`). The bytes of the file differ from ncdu's, and it holds the items of
  a scan with one thread. A scan into memory with more than one thread still peaks above one with
  one thread, by about 7 MiB on the benchmark tree and 9 to 12 MiB on `/System/Library` (see
  [BENCHMARKS.md](BENCHMARKS.md)), though the heap holds the same at the end of the scan: the system
  allocator keeps more pages resident when the tree grows while other threads allocate and free.
  Closing that gap needs control over where the tree is allocated, such as an arena or another
  global allocator, which is a larger change than the scan.
- **Errors: plain enums, and no error crate.** Every message is ncdu's, as bytes that need not be
  UTF-8, so an error carries what the caller needs to build that message, as `ScanError` does.
- **Read errors: the directory whose `readdir` failed is marked, unlike ncdu.** At `scan.zig:283`,
  ncdu 2.9.2 calls `setReadError` on the directory taken from the scan queue, which with one thread
  is always the root, not on the directory that failed. The JSON exports of both ignore which
  directory it is, so `-o` output is the same. The binary export and the in-memory tree record the
  flag per directory, so there duit flags the directory that failed, and ncdu flags the root. The
  `-2` scan screen differs the same way: `setReadError` also stores the root as `last_error` at
  `sink.zig:139`, which `drawProgress` shows as "Warning: error scanning" (`sink.zig:380-391`). duit
  names the directory that failed there.
- **JSON import: a root that is not a directory is an error, and nesting has no depth limit, unlike
  ncdu.** At `json_import.zig:519-522`, ncdu 2.9.2 crashes on a root that is written as an array but
  whose fields make it an excluded, special, or hard-linked entry, such as
  `[{"name":"/","excluded":"pattern"}]`. duit stops with `parent item must be a directory`, the
  message for a root that is not an array. ncdu reads nested directories and values by recursion, so
  a deep enough nesting overflows its stack. duit keeps a stack of its own, on the heap and with no
  limit either, so memory grows with the depth: a 6 KB `.json.zst` of 200 million `[` takes about
  400 MB. A limit would be an error that ncdu does not have.
- **Binary import: each entry has the device of its directory, unlike ncdu.** At
  `bin_reader.zig:418`, ncdu 2.9.2 passes `ctx.stat.dev` to each entry of a directory, but that
  field holds the device of the item that it read last, which after a subdirectory is the device of
  the last item inside it. An entry without a `dev` field, which is every entry on the device of its
  parent, then gets that device. Both exports then write a directory among those entries with a
  `dev` of its own, as for a directory on another filesystem. duit gives each entry the device of
  its directory.
- **Binary import: an item that is reached twice is an error, and nesting has no depth limit, unlike
  ncdu.** ncdu follows the references between items without a check: on an entry that refers to
  itself it loops forever, on a directory that contains itself it overflows its stack, and an entry
  that two directories share is written twice. duit keeps the set of the items that it read, which
  costs memory in proportion to the number of items, and stops with `Error reading item X from file`
  at the second visit. ncdu reads nested directories and skips nested CBOR values by recursion, and
  it crashes on an index block too short to hold the root reference. duit keeps stacks of its own,
  and stops with `Error reading from file` on that index. Those stacks are on the heap and have no
  limit either, so memory grows with the depth, as the set grows with the number of items. A limit
  would be an error that ncdu does not have.
- **Browsing a binary export: an entry that is reached twice in one directory is an error, unlike
  ncdu.** ncdu follows the references between the entries of a directory without a check when it
  opens the directory (`browser.zig:150-178`), so on an entry that refers back to one before it, it
  reads forever. duit stops with `Error reading item X from file` at the second visit, as an import
  does. Each directory is read into a tree of its own, so a directory that contains itself can be
  opened again and again, as in ncdu. When ncdu reads an item for the browser
  (`bin_reader.zig:441-485`), it leaves the value of a field that the type of the item does not use,
  such as the `asize` of a directory, unread when it is not an integer, and reads the next field
  from inside it. duit skips such a value.
- **Binary import: a block whose header holds another number is an error, unlike ncdu.** ncdu writes
  the number of each data block after its header, the same number as its entry in the index
  (`bin_export.zig:117-118,136`), but does not read it back (`bin_reader.zig:97`). Several entries
  of the index can then point at one block, and each of them costs a decompression of the same
  frame. duit stops with `Error reading item X from file` at such a block, which ncdu never writes.
- **Binary import and browsing: the block cache has ncdu's size, and there is no limit on the
  work.** As in ncdu (`bin_reader.zig:44,72-112`), duit keeps the 8 data blocks that it decompressed
  last. Items whose references rotate through more blocks than that decompress a block at each read,
  and a block can decompress to almost 16 MiB: a 12 KB export of 9 blocks and 9000 items takes about
  50 s on a debug build, and 6 s in ncdu. A limit on that work would be an error that ncdu does not
  have. The browser reads through the same cache when it opens a directory (`read_directory`), so a
  crafted export costs that work at each open. No budget separates such a file from one that ncdu
  writes: with `-t` above 8, each scan thread writes the entries of a shared parent into a block of
  its own (`bin_export.zig:394`), so a directory also rotates through more than 8 blocks, and
  `--export-block-size 16000` makes each of those blocks almost 16 MiB. duit writes a `-t` export
  from one thread into one series of blocks, as with `-t 1`, so its own exports never rotate a
  directory through a block per thread. The cache keeps ncdu's size anyway, as duit reads the
  exports of ncdu too.
- **Running a command: the shell is started by Rust's `std::process::Command`, unlike ncdu.** ncdu's
  `runCmd` (`ui.zig:655`) starts it with Zig's `std.process.Child`, which runs its own `PATH` search
  over `execve`. libc runs the search for Rust, and differs at two edges. A file without `#!` that
  `execve` rejects with `ENOEXEC` is run by `/bin/sh`, at least on macOS, where ncdu shows
  `Invalid executable`. An empty `NCDU_SHELL` fails with `No such file or directory`, where Zig
  tries each `PATH` directory itself and ncdu shows `Access denied`.
- **Deleting: a name with a NUL byte is an error, unlike ncdu.** Only an import can give a name a
  NUL byte. ncdu 2.9.2 reads each name up to its first NUL byte (`model.zig:90-97`), so `deleteItem`
  (`delete.zig:45-72`) removes the file or directory with the shorter name, which is another entry
  or one that is not in the tree. duit passes the whole name to `unlinkat` and `openat`, which fail
  with `EINVAL`, and shows `Unexpected` for a file and `BadPathName` for a directory. Removing the
  shorter name would delete what the user did not confirm. Refreshing a directory and running
  `--delete-command` cut the path at its first NUL byte, which gives ncdu's path when that byte is
  in the last name of the path.
- **Panics: the terminal is restored from the panic hook.** The release profile uses
  `panic = "abort"`, so `Drop` guards do not run.
- **`unsafe`: `#[expect(unsafe_code)]` plus a `SAFETY` comment.** The workspace lint warns on
  `unsafe`, and CI denies warnings.

Each of these dependencies is added to the manifest with the first code that uses it.

## Testing

ncdu is the oracle.

- **Differential tests.** A test builds a fixture tree in a temporary directory, runs `ncdu -o-` and
  `duit -o-` on it, and compares the item data. Disk usage depends on the filesystem, so the
  expected values cannot be committed: both programs run on the same machine. These tests sit behind
  a `test-ncdu` cargo feature and need ncdu 2.9.2 on `PATH`.
- **Golden files.** Exports written by the real ncdu 2.9.2 (JSON, compressed JSON, binary) are
  committed and imported by duit. These are plain files and run everywhere. ncdu writes them from a
  hand-written export rather than a scan, so that no field depends on the machine. With the same
  ncdu build, only the timestamp of the JSON exports changes when
  `crates/duit/tests/fixtures/generate.sh` writes them again.
- **Round trips.** duit writes an export, ncdu reads it, and the reverse.
- **Unit tests.** The 13 upstream `test` blocks are ported with their modules.
- **CLI tests.** `std::process::Command` with `insta` snapshots in `crates/duit/tests/it/`.
- **UI tests.** `ratatui`'s `TestBackend` with `insta` snapshots.
- **Hostile input.** The importers read untrusted files. A table of malformed inputs must produce an
  error and never a panic. The cargo-fuzz targets in `fuzz/` feed both importers arbitrary bytes.
- **Benchmarks.** `hyperfine` against ncdu on a large tree, for scan time and peak resident memory,
  recorded in `BENCHMARKS.md` from M5.

## Milestones

Each milestone ends with a working, tested command.

### M0: Scaffold

Workspace, lints, CI, and documentation. No code. Done.

### M1: Scan to JSON

- The option parser for every flag, and config files. Modes without an implementation yet (`-f`,
  `-O`, `-c`, `-t` above 1, and any run without `-o`) are rejected with a clear message. Flags that
  only change the scan progress, the browser, or the actions of M3 and M4 are parsed and have no
  effect until then, so the ncdu config files that duit falls back to keep working.
- The entry types, the sink, and a single-threaded scan with `-x`, `-L`, `-e`, exclude patterns,
  kernfs, and cache directories. `-o` writes the scan straight to the export, so the in-memory tree
  waits for M3.
- JSON export (`-o`).

Done when `duit -o- DIR` equals `ncdu -o- DIR` on the fixture trees, on Linux and macOS. Done.

### M2: Formats

- JSON import (`-f`).
- Binary export (`-O`) and the binary reader.
- zstd: `-c`, `--compress-level`, `--export-block-size`.
- Conversion between formats without a UI.

Done when round trips with ncdu pass in both directions and the malformed input table produces no
panic. Done.

### M3: Browser

- The in-memory tree (`mem_sink.zig` and the tree in `model.zig`), with hard link accounting and
  size aggregation. The browser is its first consumer.
- A read-only UI over the in-memory tree and over a binary export: navigation, sorting, columns,
  info, help, quit, colors, and every display flag.

Done when the snapshot tests pass and a manual side-by-side checklist against ncdu is complete.
Done.

### M4: Actions

- Scan progress screens: `-0`, `-1`, `-2`, `-q`.
- Refresh, delete with confirmation and `--delete-command`, and shell.
- Read-only modes (`-r`, `-rr`) and the enable and disable flags.

Done when every flag in `ncdu.1` and every key works. This is parity. Done.

### M5: Performance

Scan threads (`-t`), the memory layout decision, and benchmarks. A JSON export (`-o`) of a scan with
more than one thread is written from the in-memory tree after the scan, which needs `mem_src.zig`.

Done when scan time and peak memory are at or below ncdu's, or the gap is documented. Done.

### M6: Release

Man page, changelog, prebuilt binaries with cargo-dist for Linux and macOS, crates.io, and fuzzing.

Done when `0.1.0` is published.

M1 comes first because it has no UI: its output can be compared with ncdu's by a machine from the
first day.

## Open questions

1. **Config file location.** Decided: read `/etc/duit.conf` and `$XDG_CONFIG_HOME/duit/config`, or
   `$HOME/.config/duit/config` if `XDG_CONFIG_HOME` is not set. If neither exists, read ncdu's files
   (`/etc/ncdu.conf`, and the user file under `ncdu/`) instead. The fallback is all or nothing: one
   duit file, even an empty one, turns off both ncdu files.
2. **`progname` in exports.** Decided for JSON: write `duit`. ncdu 2.9.2 imports such an export, as
   the `test-ncdu` round trip in `duit-format` checks. The binary export has no metadata block, so
   it holds no `progname`: ncdu 2.9.2 reads duit's binary export, as the `test-ncdu` tests of `-O`
   in `duit` check.
3. **Invocation as `ncdu`.** Should a symlink named `ncdu` change any behavior? Proposal: no.
4. **Defaults.** Drop-in compatibility means ncdu's defaults, including one scan thread. Better
   defaults would need an explicit opt-in and wait until after M4.

## License and attribution

duit is a derived work of ncdu, which is MIT licensed. `LICENSE` carries the upstream copyright
notice next to duit's own.
