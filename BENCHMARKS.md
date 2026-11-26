# Benchmarks

Scan time and peak memory of duit against ncdu 2.9.2, for the goal in [PLAN.md](PLAN.md): scan time
and peak memory at or below ncdu's, with peak memory at most 1.25 times ncdu's for the tree in
memory.

## Setup

- Machine: Apple M5 Max, 18 logical CPUs, 36 GiB of memory, internal SSD with APFS
- OS: macOS 27.2.1 (Darwin 27.2.0, arm64)
- Versions: ncdu 2.9.2 (Homebrew), duit 0.0.1 at the commit of this file (release profile),
  hyperfine 2.0.0
- Runs: 3 warmup and 10 timed runs per command with `hyperfine -N`, so the caches are warm. Peak
  memory is the median of 5 runs of `/usr/bin/time -l` (`-v` on Linux), as the maximum resident set
  size.

The generated tree has 992,818 entries: a directory of 100,000 files, a tree of 1,000 directories
with 880 files each where every tenth file is a hard link (about 89,000 links, some of them across
directories), 10,000 names of 100 to 255 bytes, and a chain of 64 nested directories. One file in 16
has data, from 1 to 1,000 bytes. The script checks that duit and ncdu write the same entries for it
before it measures anything.

Every command starts with `--ignore-config -0`, and both programs get the same flags:

- Scan into memory: `-t N --quit-after-scan TREE`
- Scan to JSON: `-t N -o /dev/null TREE`
- Scan to binary: `-t N -O /dev/null TREE`
- Import JSON into memory: `-f tree.json --quit-after-scan`, an export of the tree by ncdu
- Import binary to JSON: `-f tree.ncdu -o /dev/null`, an export of the tree by ncdu. With
  `--quit-after-scan`, both programs return before they read a binary export, because the browser
  reads it on demand, so this case converts it instead.
- A real tree, `/System/Library` (468,611 entries): `-x -t N --quit-after-scan /System/Library`

## Rerun

With Python 3, [hyperfine](https://github.com/sharkdp/hyperfine), and ncdu 2.9.2 on `PATH`:

```shell
make bench
```

`python3 scripts/benchmark.py --help` lists the options: another ncdu binary, the number of runs,
the values of `-t`, and the real tree. The first run generates the tree in `target/benchmark/`, in
about 40 seconds. A run takes about 20 minutes and writes the table to
`target/benchmark/results.md`.

## Results

The ratios are duit divided by ncdu, so below 1 is better for duit.

| Case                                       |       ncdu time |       duit time | Time ratio | ncdu peak RSS | duit peak RSS | RSS ratio |
| ------------------------------------------ | --------------: | --------------: | ---------: | ------------: | ------------: | --------: |
| Scan into memory, `-t 1`                   | 4.984 s ± 0.078 | 4.902 s ± 0.048 |       0.98 |      45.3 MiB |      53.6 MiB |      1.18 |
| Scan into memory, `-t 4`                   | 2.152 s ± 0.036 | 2.136 s ± 0.003 |       0.99 |      45.3 MiB |      89.7 MiB |      1.98 |
| Scan into memory, `-t 8`                   | 2.351 s ± 0.059 | 2.365 s ± 0.024 |       1.01 |      45.4 MiB |      94.6 MiB |      2.08 |
| Scan to JSON, `-t 1`                       | 4.947 s ± 0.102 | 4.849 s ± 0.009 |       0.98 |       2.7 MiB |       2.9 MiB |      1.08 |
| Scan to JSON, `-t 4`                       | 2.132 s ± 0.033 | 2.134 s ± 0.005 |       1.00 |      45.4 MiB |      97.1 MiB |      2.14 |
| Scan to JSON, `-t 8`                       | 2.370 s ± 0.045 | 2.351 s ± 0.009 |       0.99 |      45.5 MiB |     102.4 MiB |      2.25 |
| Scan to binary, `-t 1`                     | 4.950 s ± 0.020 | 4.862 s ± 0.019 |       0.98 |       3.9 MiB |       4.2 MiB |      1.06 |
| Scan to binary, `-t 4`                     | 2.127 s ± 0.030 | 2.094 s ± 0.041 |       0.98 |       5.1 MiB |      39.9 MiB |      7.82 |
| Scan to binary, `-t 8`                     | 2.329 s ± 0.063 | 2.337 s ± 0.045 |       1.00 |       7.5 MiB |      44.2 MiB |      5.94 |
| Import JSON into memory                    | 0.082 s ± 0.000 | 0.142 s ± 0.004 |       1.73 |      45.2 MiB |      53.5 MiB |      1.18 |
| Import binary to JSON                      | 0.081 s ± 0.000 | 0.100 s ± 0.001 |       1.23 |       3.4 MiB |      39.3 MiB |     11.64 |
| Scan `/System/Library` into memory, `-t 1` | 4.708 s ± 0.022 | 4.677 s ± 0.013 |       0.99 |      28.9 MiB |      28.9 MiB |      1.00 |
| Scan `/System/Library` into memory, `-t 8` | 1.693 s ± 0.013 | 1.850 s ± 0.066 |       1.09 |      29.2 MiB |      39.1 MiB |      1.34 |

## Gaps

### Slower than ncdu

- Import JSON into memory: 1.73 times ncdu's time. The import is CPU bound, so a scan of the same
  tree, which waits on `stat`, does not show it.
- Import binary to JSON: 1.23 times ncdu's time.
- Scan `/System/Library` into memory with `-t 8`: 1.09 times ncdu's time.
- Scans of the generated tree into memory with `-t 8`, to JSON with `-t 4`, and to binary with
  `-t 8`: up to 1% slower, within one standard deviation of ncdu's time.

The other scans take the same time as ncdu's or less.

### More memory than ncdu

duit uses more memory than ncdu in every case but one: the scan of `/System/Library` with `-t 1`
takes the same. Every case with `-t 1` is within the budget of 1.25 times ncdu's peak memory, and so
is the import of JSON. The cases with several threads and the import of binary to JSON are over it.

- The tree in memory takes 1.18 times as much as ncdu's for a scan or an import of the generated
  tree with `-t 1`, and the same for `/System/Library`. This is the measure for the memory layout
  decision of M5 in PLAN.md: an entry takes 40 bytes, a directory 40 more, and a hard link 32 more.
  The two scans that stream to an export with `-t 1` take 1.06 and 1.08 times ncdu's peak.
- A scan with several threads adds more on top: 90 MiB with `-t 4` against 54 MiB with `-t 1`, while
  ncdu stays at 45 MiB. A scan to JSON with several threads goes through the tree in memory in both
  programs, so it has about the same peak as the scan into memory.
- A scan to binary with several threads takes 40 to 44 MiB, against 4.2 MiB with `-t 1`. ncdu goes
  from 3.9 MiB to 5.1 and 7.5 MiB.
- Import binary to JSON takes 39.3 MiB, 11.64 times ncdu's 3.4 MiB.

## Performance bugs for later

These are left as they are here. A heap or CPU profile of a build with the `profiling` profile
(release with debug information and without LTO, `cargo build --profile profiling --bin duit`) is
the place to start, for example with Instruments (Allocations or Time Profiler) on macOS or
`heaptrack` and `perf` on Linux.

- **Import binary to JSON keeps a set of every item.** `BinaryReader::import` in
  `crates/duit-format/src/bin_reader.rs` inserts each item reference into a `HashSet<u64>` named
  `visited`, to reject references in a cycle. For about a million items, its table of 2^21 buckets
  takes about 18 MiB, and up to 27 MiB while it grows, which is likely most of the 36 MiB over ncdu,
  whose `import` in `bin_reader.zig` keeps nothing per item. The cycle check needs a bound that does
  not grow with the number of items.
- **Memory grows with threads in a scan to binary.** From `-t 1` to `-t 4`, duit's peak goes from
  4.2 to 39.8 MiB on a tree of a million entries, about 38 bytes per entry, which points at state
  that is kept per entry or per directory until the end of the scan instead of written out. Profile
  `duit --ignore-config -0 -t 4 -O /dev/null TREE`.
- **The tree in memory grows with threads.** A scan into memory with `-t 4` peaks 36 MiB above one
  with `-t 1`, for the same tree. Profile `duit --ignore-config -0 -t 4 --quit-after-scan TREE`
  against `-t 1`.
- **JSON import is CPU bound and 1.73 times slower.** Profile
  `duit --ignore-config -0 -f tree.json --quit-after-scan`, with the importer in
  `crates/duit-format/src/json_import.rs` and the tree in `crates/duit-model`.
