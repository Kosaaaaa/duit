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
| Scan into memory, `-t 1`                   | 6.248 s ± 1.143 | 4.980 s ± 0.086 |       0.80 |      45.3 MiB |      53.7 MiB |      1.19 |
| Scan into memory, `-t 4`                   | 2.175 s ± 0.047 | 2.176 s ± 0.041 |       1.00 |      45.3 MiB |      61.0 MiB |      1.35 |
| Scan into memory, `-t 8`                   | 2.380 s ± 0.067 | 2.358 s ± 0.040 |       0.99 |      45.4 MiB |      60.8 MiB |      1.34 |
| Scan to JSON, `-t 1`                       | 4.913 s ± 0.055 | 4.827 s ± 0.047 |       0.98 |       2.7 MiB |       2.9 MiB |      1.08 |
| Scan to JSON, `-t 4`                       | 2.396 s ± 0.182 | 2.389 s ± 0.149 |       1.00 |      44.9 MiB |      68.5 MiB |      1.53 |
| Scan to JSON, `-t 8`                       | 2.827 s ± 0.765 | 3.172 s ± 0.324 |       1.12 |      44.2 MiB |      67.2 MiB |      1.52 |
| Scan to binary, `-t 1`                     | 5.086 s ± 0.142 | 4.903 s ± 0.049 |       0.96 |       4.8 MiB |       4.2 MiB |      0.87 |
| Scan to binary, `-t 4`                     | 2.273 s ± 0.150 | 2.160 s ± 0.048 |       0.95 |       5.1 MiB |       5.0 MiB |      0.98 |
| Scan to binary, `-t 8`                     | 2.348 s ± 0.059 | 2.351 s ± 0.043 |       1.00 |       7.5 MiB |       7.2 MiB |      0.97 |
| Import JSON into memory                    | 0.087 s ± 0.001 | 0.142 s ± 0.001 |       1.63 |      45.2 MiB |      53.5 MiB |      1.18 |
| Import binary to JSON                      | 0.082 s ± 0.000 | 0.102 s ± 0.001 |       1.24 |       3.4 MiB |      39.3 MiB |     11.63 |
| Scan `/System/Library` into memory, `-t 1` | 4.978 s ± 0.869 | 4.744 s ± 0.132 |       0.95 |      28.9 MiB |      28.9 MiB |      1.00 |
| Scan `/System/Library` into memory, `-t 8` | 1.717 s ± 0.027 | 1.932 s ± 0.049 |       1.13 |      29.3 MiB |      40.8 MiB |      1.39 |

## Gaps

### Slower than ncdu

- Import JSON into memory: 1.63 times ncdu's time. The import is CPU bound, so a scan of the same
  tree, which waits on `stat`, does not show it.
- Import binary to JSON: 1.24 times ncdu's time.
- Scan `/System/Library` into memory with `-t 8`: 1.13 times ncdu's time.
- Scan of the generated tree to JSON with `-t 8`: 1.12 times ncdu's time, with a standard deviation
  of 0.3 to 0.8 seconds on both sides, as other work ran on the machine during that case.

The other scans take the same time as ncdu's or less.

### More memory than ncdu

Every case with `-t 1` is within the budget of 1.25 times ncdu's peak memory, and so are the import
of JSON and the scans to binary with several threads. The scans into memory or to JSON with several
threads and the import of binary to JSON are over it.

- The tree in memory takes 1.19 times as much as ncdu's for a scan or an import of the generated
  tree with `-t 1`, and the same for `/System/Library`. This is the measure for the memory layout
  decision of M5 in PLAN.md: an entry takes 40 bytes, a directory 40 more, and a hard link 32 more.
  The two scans that stream to an export with `-t 1` take 1.08 and 0.87 times ncdu's peak. ncdu's
  peak for the scan to binary went from 3.9 to 5.7 MiB between runs, while duit's stayed at 4.2 MiB.
- A scan to binary with several threads takes 5.0 MiB with `-t 4` and 7.2 MiB with `-t 8`, at or
  below ncdu's 5.1 and 7.5 MiB. The threads send each directory in parts of at most 64 entries, so a
  directory of 100,000 entries no longer waits whole for the calling thread.
- A scan into memory with several threads peaks at 61 MiB against 54 MiB with `-t 1`, 1.35 times
  ncdu's 45 MiB, and `/System/Library` with `-t 8` at 40.8 MiB against 28.9 MiB, 1.39 times ncdu's.
  The heap holds the same at the end of the scan as with `-t 1`, so the difference is in the pages
  that the system allocator keeps resident when the tree grows while other threads allocate and
  free. See the Threads decision in PLAN.md.
- A scan to JSON with several threads goes through the tree in memory in both programs, and takes 67
  to 69 MiB, 1.5 times ncdu's peak: the peak of the scan into memory, and about 7 MiB more to write
  the export from the tree.
- Import binary to JSON takes 39.3 MiB, 11.63 times ncdu's 3.4 MiB. `BinaryReader::import` in
  `crates/duit-format/src/bin_reader.rs` inserts each item reference into a `HashSet<u64>` named
  `visited`, to reject an item that is reached twice. For about a million items, its table of 2^21
  buckets takes about 18 MiB, and up to 27 MiB while it grows, which is likely most of the 36 MiB
  over ncdu, whose `import` in `bin_reader.zig` keeps nothing per item. PLAN.md accepts that memory
  in its decision "Binary import: an item that is reached twice is an error, and nesting has no
  depth limit, unlike ncdu".

## Performance bugs for later

These are left as they are here. A heap or CPU profile of a build with the `profiling` profile
(release with debug information and without LTO, `cargo build --profile profiling --bin duit`) is
the place to start, for example with Instruments (Allocations or Time Profiler) on macOS or
`heaptrack` and `perf` on Linux.

- **A scan into memory with several threads keeps more pages resident.** With `-t 2` to `-t 8`, a
  scan of the generated tree into memory peaks 6 to 8 MiB above one with `-t 1`, whatever the size
  of the parts that the threads send. The allocations of 256 KiB or more are the same in size and
  order, and so is the heap at the end of the scan, but the peak physical footprint is higher too,
  and `vmmap` shows the difference in the regions of the system allocator. Closing it needs control
  over where the tree is allocated, such as an arena for the tree or another global allocator.
  Profile `duit --ignore-config -0 -t 4 --quit-after-scan TREE` against `-t 1`.
- **The queue of a scan with several threads grows with the breadth of the tree.** For
  `/System/Library` with `-t 8`, the heap peaks about 2 MiB above `-t 1`, likely in the directories
  that wait in the queue, last in first out, and the directories that are open until their
  subdirectories are finished.
- **JSON import is CPU bound and 1.63 times slower.** Profile
  `duit --ignore-config -0 -f tree.json --quit-after-scan`, with the importer in
  `crates/duit-format/src/json_import.rs` and the tree in `crates/duit-model`.
