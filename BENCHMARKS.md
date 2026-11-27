# Benchmarks

Scan time and peak memory of duit against ncdu 2.9.2, for milestone M8 in [PLAN.md](PLAN.md): less
time and less peak memory than ncdu in every case.

## Setup

- Machine: Apple M5 Max, 18 logical CPUs, 36 GiB of memory, internal SSD with APFS
- OS: macOS 27.2.1 (Darwin 27.2.0, arm64)
- Versions: ncdu 2.9.2 (Homebrew), duit 0.1.0 at the commit of this file (release profile),
  hyperfine 2.0.0
- Load: other programs used 1 to 2 CPUs during the run (a load average of 6 to 7), which shows in
  the standard deviations of the scans with one thread.
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
| Scan into memory, `-t 1`                   | 4.723 s ± 0.303 | 5.182 s ± 0.115 |       1.10 |      45.3 MiB |      34.1 MiB |      0.75 |
| Scan into memory, `-t 4`                   | 2.301 s ± 0.171 | 2.191 s ± 0.074 |       0.95 |      45.4 MiB |      39.7 MiB |      0.87 |
| Scan into memory, `-t 8`                   | 2.368 s ± 0.099 | 2.333 s ± 0.113 |       0.99 |      45.5 MiB |      41.0 MiB |      0.90 |
| Scan to JSON, `-t 1`                       | 4.984 s ± 0.193 | 4.851 s ± 0.302 |       0.97 |       2.7 MiB |       3.0 MiB |      1.10 |
| Scan to JSON, `-t 4`                       | 2.211 s ± 0.030 | 2.282 s ± 0.138 |       1.03 |      45.4 MiB |      43.5 MiB |      0.96 |
| Scan to JSON, `-t 8`                       | 2.341 s ± 0.083 | 2.358 s ± 0.049 |       1.01 |      45.5 MiB |      43.5 MiB |      0.96 |
| Scan to binary, `-t 1`                     | 5.167 s ± 0.154 | 5.267 s ± 0.688 |       1.02 |       4.8 MiB |       4.3 MiB |      0.89 |
| Scan to binary, `-t 4`                     | 2.254 s ± 0.070 | 2.151 s ± 0.057 |       0.95 |       5.9 MiB |       5.1 MiB |      0.87 |
| Scan to binary, `-t 8`                     | 2.333 s ± 0.056 | 2.266 s ± 0.073 |       0.97 |       7.4 MiB |       6.3 MiB |      0.85 |
| Import JSON into memory                    | 0.094 s ± 0.006 | 0.096 s ± 0.006 |       1.02 |      45.2 MiB |      33.9 MiB |      0.75 |
| Import binary to JSON                      | 0.085 s ± 0.002 | 0.074 s ± 0.009 |       0.86 |       3.9 MiB |       3.7 MiB |      0.95 |
| Scan `/System/Library` into memory, `-t 1` | 4.617 s ± 0.055 | 4.629 s ± 0.036 |       1.00 |      28.9 MiB |      22.3 MiB |      0.77 |
| Scan `/System/Library` into memory, `-t 8` | 1.708 s ± 0.012 | 1.785 s ± 0.017 |       1.05 |      29.3 MiB |      28.5 MiB |      0.97 |

## Gaps

M8 is not done: one row takes more peak memory than ncdu, and several rows take as much time or
more.

### More memory than ncdu

- Scan to JSON with `-t 1`: 3.0 MiB, 1.10 times ncdu's 2.7 MiB. The export streams, and the heap of
  both programs stays near 0.1 MiB. With `vmmap`, the dirty memory of both is within 64 KiB, and the
  difference is in the code that is resident: 464 KiB of duit's own `__TEXT`, against 208 KiB of
  ncdu's and 32 KiB of its `libzstd` and `libncursesw`. Built with `opt-level = "s"`, duit keeps 64
  KiB less resident and imports JSON 15% slower. Closing it needs code that the scan and the export
  touch to fit on fewer pages, which is neither a question of `unsafe` nor of output. The same cost
  is in the import of binary to JSON, at 0.95 here and 1.00 in an earlier run, where ncdu's own peak
  moved from 3.25 to 3.9 MiB between runs.

### Slower than ncdu

- The scans with one thread take the time of ncdu's, within their standard deviations of 0.03 to 0.7
  s: they wait on `fstatat`, once for each entry in both programs. Run back to back on a less loaded
  machine, the scan into memory with `-t 1` took 4.888 s against ncdu's 4.948 s, and the scan to
  JSON 5.095 s ± 0.269 against 4.900 s ± 0.212.
- Scan of `/System/Library` with `-t 8`: 1.05 times ncdu's time, and 1.00 with `-t 1`. A sample of
  the threads shows them in system calls, with the calling thread waiting for parts. About 4% of the
  samples are `fstat`, `fstatfs`, and `fcntl` calls that ncdu does not make: rustix opens each
  directory for reading with libc's `fdopendir`, which makes them, where ncdu reads with
  `getdirentries` itself. The tree has 163,817 directories, so this shows here and not on the
  generated tree, which has 1,179. Reading with `getdirentries` needs `unsafe` code, which M8 rules
  out.
- Scans to JSON with `-t 4` and `-t 8` (1.03 and 1.01) and the import of JSON (1.02, against 0.88 in
  an earlier run at 0.085 s for ncdu and 0.074 s for duit) are within the noise of this run.

## Changes for M8, and what did not help

- Import binary to JSON took 11.63 times ncdu's memory, for a set of the items read so far. The
  import now keeps only the last reference while each item has a lower one than the item before,
  which is the order of an export with one thread, and collects the set only at the first item that
  breaks it, as in an export with several threads.
- The JSON import reads a byte through an inlined fast path, copies the plain bytes of a string at
  once, and reuses its key buffer. The hard link accounting hashes directory identifiers with a
  multiplication and reuses its maps: import JSON into memory went from 0.142 s to 0.074 s.
- An entry of the tree takes 24 bytes instead of 40, a directory 20 instead of 40, and a hard link
  28 instead of 32. See the Tree memory decision in PLAN.md.
- The scan threads read depth-first with a queue of 16 directories, as ncdu's do, so the directories
  that wait stay few. See the Threads decision in PLAN.md.
- The tree of a JSON export with several threads grows in chunks that never move: the export reads
  all of it, and the freed buffers of a doubling `Vec` stayed resident. Other trees grow like a
  `Vec`, because on macOS the pages that a large `realloc` moves only count again once touched.
- Measured, and left out: a pool of the parts that the scan threads send (no change in time or peak
  memory, measured twice), mimalloc as the global allocator (102 MiB for the scan into memory with
  `-t 1`, against 53.7 MiB at the time), reserving the whole tree up front (63 MiB against 53.7 MiB
  with the entries of 40 bytes), and `opt-level = "s"` (see above).

## Performance bugs for later

A heap or CPU profile of a build with the `profiling` profile (release with debug information and
without LTO, `cargo build --profile profiling --bin duit`) is the place to start, for example with
`sample` and `vmmap` on macOS, or `heaptrack` and `perf` on Linux.

- **The resident code of a streaming export.** See "More memory than ncdu".
- **`fdopendir` for each directory.** See "Slower than ncdu".
- **The peak memory of a scan with several threads moves between runs.** For `/System/Library` with
  `-t 8`, it ranged from 27.0 to 29.5 MiB over 11 runs, where ncdu's stays at 29.2 MiB. At the end
  of the scan, the small regions of the system allocator hold about 10 MiB with only 0.12 MiB live,
  memory that the threads allocated and the calling thread freed.
