# Benchmarks

Scan time and peak memory of duit against ncdu 2.9.2, for milestone M8 in [PLAN.md](PLAN.md): less
time and less peak memory than ncdu in every case.

## Setup

- Machine: Apple M5 Max, 18 logical CPUs, 36 GiB of memory, internal SSD with APFS
- OS: macOS 27.2.1 (Darwin 27.2.0, arm64)
- Versions: ncdu 2.9.2 (Homebrew), duit 0.1.0 at the commit of this file (release profile),
  hyperfine 2.0.0
- Load: a load average of 0.9 at the start of the run and 6.6 at its end.
- Runs: 3 warmup and 10 timed runs per command with `hyperfine -N`, so the caches are warm. Peak
  memory is the median of 5 runs of `/usr/bin/time -l` (`-v` on Linux), as the maximum resident set
  size.

The generated tree has 992,818 entries: a directory of 100,000 files, a tree of 1,000 directories
with 880 files each where every tenth file is a hard link (178,000 links to about 89,000 inodes,
some of them across directories), 10,000 names of 100 to 255 bytes, and a chain of 64 nested
directories. One file in 16 has data, from 1 to 1,000 bytes. The script checks that duit and ncdu
write the same entries for it before it measures anything.

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
| Scan into memory, `-t 1`                   | 5.039 s ± 0.018 | 4.950 s ± 0.015 |       0.98 |      45.3 MiB |      34.1 MiB |      0.75 |
| Scan into memory, `-t 4`                   | 2.134 s ± 0.026 | 2.087 s ± 0.039 |       0.98 |      45.3 MiB |      39.3 MiB |      0.87 |
| Scan into memory, `-t 8`                   | 2.347 s ± 0.072 | 2.300 s ± 0.058 |       0.98 |      45.5 MiB |      40.8 MiB |      0.90 |
| Scan to JSON, `-t 1`                       | 4.999 s ± 0.034 | 4.857 s ± 0.040 |       0.97 |       2.7 MiB |       3.0 MiB |      1.10 |
| Scan to JSON, `-t 4`                       | 2.148 s ± 0.049 | 2.127 s ± 0.018 |       0.99 |      45.4 MiB |      43.3 MiB |      0.95 |
| Scan to JSON, `-t 8`                       | 2.280 s ± 0.098 | 2.280 s ± 0.051 |       1.00 |      45.5 MiB |      44.1 MiB |      0.97 |
| Scan to binary, `-t 1`                     | 5.063 s ± 0.016 | 4.934 s ± 0.011 |       0.97 |       3.9 MiB |       4.3 MiB |      1.09 |
| Scan to binary, `-t 4`                     | 2.107 s ± 0.033 | 2.072 s ± 0.024 |       0.98 |       5.1 MiB |       5.1 MiB |      1.00 |
| Scan to binary, `-t 8`                     | 2.324 s ± 0.048 | 2.264 s ± 0.079 |       0.97 |       7.4 MiB |       5.5 MiB |      0.75 |
| Import JSON into memory                    | 0.084 s ± 0.001 | 0.079 s ± 0.003 |       0.93 |      45.3 MiB |      33.8 MiB |      0.75 |
| Import binary to JSON                      | 0.081 s ± 0.000 | 0.066 s ± 0.000 |       0.82 |       3.4 MiB |       3.4 MiB |      1.01 |
| Scan `/System/Library` into memory, `-t 1` | 4.646 s ± 0.025 | 4.638 s ± 0.008 |       1.00 |      29.0 MiB |      22.3 MiB |      0.77 |
| Scan `/System/Library` into memory, `-t 8` | 1.701 s ± 0.014 | 1.767 s ± 0.010 |       1.04 |      29.3 MiB |      28.9 MiB |      0.99 |

## Gaps

M8 is not done: four rows take more peak memory than ncdu, or as much, and three take as much time
or more: the scan to JSON with `-t 8` and both scans of `/System/Library`.

### More memory than ncdu

- Scan to JSON with `-t 1` (1.10), scan to binary with `-t 1` (1.09), and import binary to JSON
  (1.01). These stream, so the heap of both programs stays below 1 MiB, and the difference is in the
  code that is resident. With `vmmap` during the scan to JSON, the dirty memory of both is within 64
  KiB, but 464 KiB of duit's own `__TEXT` is resident, against 208 KiB of ncdu's and 32 KiB of its
  `libzstd` and `libncursesw`. Built with `opt-level = "s"`, duit keeps 64 KiB less resident and
  imports JSON 15% slower. The peak of ncdu's scan to binary moves between runs, from 3.9 to 4.8 MiB
  in the last two, where duit's stays at 4.2 to 4.3 MiB. Closing this needs the code that these
  paths touch to fit on fewer pages: it needs neither `unsafe` code nor other output, so it is open
  work, not a limit.
- Scan to binary with `-t 4` (1.00): both programs peak at 5.1 MiB. Not profiled yet.
- Scan of `/System/Library` with `-t 8`: 0.99 here, but its peak moves between runs, from 26.6 to
  29.8 MiB in this run and from 27.0 to 31.1 MiB over 11 runs each in two builds before it, where
  ncdu's stays at 29.2 MiB. The heap at the end of the scan is the same as with `-t 1`, and the
  small regions of the system allocator hold memory that the threads allocated and the calling
  thread freed. A pool of the parts that the threads send did not change it.

### Slower than ncdu

- Scan of `/System/Library` with `-t 1` (1.00): it waits on `fstatat`, once for each entry in both
  programs, and to take less time it would need fewer system calls, which on macOS means reading a
  directory with its attributes by `getattrlistbulk`, or with `getdirentries` instead of `readdir`.
  rustix 1.1.6 wraps neither on Apple platforms (its `RawDir` is for Linux only), so either needs
  `unsafe` code, which M8 rules out.
- Scan of `/System/Library` with `-t 8` (1.04). A sample of the threads with `-t 8` shows them in
  system calls, with the calling thread waiting for parts, and about 4% of the samples in `fstat`,
  `fstatfs`, and `fcntl` calls that ncdu does not make: rustix opens each directory for reading with
  libc's `fdopendir`, which makes them, where ncdu calls `__getdirentries64`, a private symbol of
  libSystem, directly. The tree has 163,817 directories, so this shows here and not on the generated
  tree, which has 1,179. As above, reading without `fdopendir` needs `unsafe` code.
- Scan to JSON with `-t 8` (1.00) is within the noise of the scans with several threads, which take
  0.97 to 0.99 in the other rows.

## Changes for M8, and what did not help

- Import binary to JSON took 11.63 times ncdu's memory, for a set of the items read so far. The
  import now keeps only the last reference while each item has a lower one than the item before,
  which is the order of an export with one thread, and collects the set only at the first item that
  breaks it, as in an export with several threads.
- The JSON import reads a byte through an inlined fast path, copies the plain bytes of a string at
  once, and reuses its key buffer. The hard link accounting hashes directory identifiers with a
  multiplication and reuses its maps. Import JSON into memory went from 0.142 s to 0.079 s.
- An entry of the tree takes 24 bytes instead of 40, a directory 20 instead of 40, and a hard link
  28 instead of 32. See the Tree memory decision in PLAN.md.
- The scan threads read depth-first with a queue of 16 directories, as ncdu's do, so the directories
  that wait stay few. See the Threads decision in PLAN.md.
- The tree of a JSON export with several threads grows in chunks that never move. Measured, a tree
  whose lists double peaks above its data once all of it is read, as that export does, and below it
  otherwise, so other trees still grow like a `Vec`.
- Measured, and left out: a pool of the parts that the scan threads send (no change in time or peak
  memory, measured three times), mimalloc as the global allocator (102 MiB for the scan into memory
  with `-t 1`, against 53.7 MiB at the time), reserving the whole tree up front (63 MiB against 53.7
  MiB with the entries of 40 bytes), and `opt-level = "s"` (see above).

## Performance bugs for later

A heap or CPU profile of a build with the `profiling` profile (release with debug information and
without LTO, `cargo build --profile profiling --bin duit`) is the place to start, for example with
`sample` and `vmmap` on macOS, or `heaptrack` and `perf` on Linux.

- **The resident code of a streaming scan or conversion.** See "More memory than ncdu".
- **The peak memory of a scan with several threads moves between runs.** See "More memory than
  ncdu".
- **`fdopendir` for each directory, and one `fstatat` for each entry.** See "Slower than ncdu".
