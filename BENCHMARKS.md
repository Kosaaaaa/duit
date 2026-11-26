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
| Scan into memory, `-t 1`                   | 4.974 s ± 0.042 | 4.895 s ± 0.048 |       0.98 |      45.3 MiB |      88.4 MiB |      1.95 |
| Scan into memory, `-t 4`                   | 2.138 s ± 0.039 | 2.157 s ± 0.036 |       1.01 |      45.3 MiB |     136.8 MiB |      3.02 |
| Scan into memory, `-t 8`                   | 2.316 s ± 0.088 | 2.359 s ± 0.006 |       1.02 |      45.4 MiB |     133.4 MiB |      2.94 |
| Scan to JSON, `-t 1`                       | 4.983 s ± 0.058 | 4.886 s ± 0.034 |       0.98 |       2.7 MiB |       2.9 MiB |      1.08 |
| Scan to JSON, `-t 4`                       | 2.129 s ± 0.034 | 2.153 s ± 0.063 |       1.01 |      45.4 MiB |     135.8 MiB |      2.99 |
| Scan to JSON, `-t 8`                       | 2.324 s ± 0.079 | 2.363 s ± 0.030 |       1.02 |      45.5 MiB |     138.9 MiB |      3.05 |
| Scan to binary, `-t 1`                     | 4.997 s ± 0.054 | 4.902 s ± 0.011 |       0.98 |       3.9 MiB |       4.2 MiB |      1.08 |
| Scan to binary, `-t 4`                     | 2.120 s ± 0.030 | 2.100 s ± 0.005 |       0.99 |       5.9 MiB |      39.8 MiB |      6.72 |
| Scan to binary, `-t 8`                     | 2.316 s ± 0.075 | 2.327 s ± 0.046 |       1.00 |       7.4 MiB |      44.2 MiB |      5.96 |
| Import JSON into memory                    | 0.084 s ± 0.003 | 0.149 s ± 0.001 |       1.78 |      45.3 MiB |      88.2 MiB |      1.95 |
| Import binary to JSON                      | 0.081 s ± 0.000 | 0.100 s ± 0.001 |       1.23 |       3.4 MiB |      39.3 MiB |     11.65 |
| Scan `/System/Library` into memory, `-t 1` | 4.711 s ± 0.030 | 4.698 s ± 0.035 |       1.00 |      28.8 MiB |      49.6 MiB |      1.72 |
| Scan `/System/Library` into memory, `-t 8` | 1.693 s ± 0.013 | 1.909 s ± 0.054 |       1.13 |      29.2 MiB |      56.4 MiB |      1.93 |

## Gaps

### Slower than ncdu

- Import JSON into memory: 1.78 times ncdu's time. The import is CPU bound, so a scan of the same
  tree, which waits on `stat`, does not show it.
- Import binary to JSON: 1.23 times ncdu's time.
- Scan `/System/Library` into memory with `-t 8`: 1.13 times ncdu's time.
- Scans of the generated tree with `-t 4` and `-t 8` into memory and to JSON, and with `-t 8` to
  binary: up to 2% slower, within one standard deviation of ncdu's time.

The single-threaded scans take the same time as ncdu's or less, and so does the scan to binary with
`-t 4`.

### More memory than ncdu

duit uses more memory than ncdu in every case. Every case is also over the budget of 1.25 times
ncdu's peak memory, except the two that stream a single-threaded scan to an export: scan to JSON and
scan to binary with `-t 1`, at 1.08 times.

- The tree in memory takes about twice as much as ncdu's: 1.95 times for a scan or an import of the
  generated tree with `-t 1`, and 1.72 times for `/System/Library`. This is the measure for the
  memory layout decision of M5 in PLAN.md.
- A scan with several threads adds more on top: 137 MiB with `-t 4` against 88 MiB with `-t 1`,
  while ncdu stays at 45 MiB. A scan to JSON with several threads goes through the tree in memory in
  both programs, so it has the same peak as the scan into memory.
- A scan to binary with several threads takes 40 to 44 MiB, against 4.2 MiB with `-t 1`. ncdu goes
  from 3.9 MiB to 5.9 and 7.4 MiB.
- Import binary to JSON takes 39.3 MiB, 11.65 times ncdu's 3.4 MiB.

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
- **The tree in memory grows with threads.** A scan into memory with `-t 4` peaks 48 MiB above one
  with `-t 1`, for the same tree. Profile `duit --ignore-config -0 -t 4 --quit-after-scan TREE`
  against `-t 1`.
- **JSON import is CPU bound and 1.78 times slower.** Profile
  `duit --ignore-config -0 -f tree.json --quit-after-scan`, with the importer in
  `crates/duit-format/src/json_import.rs` and the tree in `crates/duit-model`.
