# Benchmarks

Scan time and peak memory of duit against ncdu 2.9.2, for milestone M8 in [PLAN.md](PLAN.md): less
time and less peak memory than ncdu in every case.

## Setup

- Machine: Apple M5 Max, 18 logical CPUs, 36 GiB of memory, internal SSD with APFS
- OS: macOS 27.2.1 (Darwin 27.2.0, arm64)
- Versions: ncdu 2.9.2 (Homebrew), duit 0.1.2 at the commit of this file (release profile),
  hyperfine 2.0.0
- Load: a load average of 0.8 before the run and of 7.4 right after it, which still counts the 8
  threads of the last case.
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
- A real tree, `/System/Library` (469,671 entries): `-x -t N --quit-after-scan /System/Library`

## Rerun

With [uv](https://docs.astral.sh/uv/), [hyperfine](https://github.com/sharkdp/hyperfine), ncdu 2.9.2
on `PATH`, and `/usr/bin/time` (GNU time on Linux):

```shell
make bench
```

`uvx --python '~=3.14.0' python scripts/benchmark.py --help` lists the options: another ncdu binary,
the number of runs, the values of `-t`, and the real tree. The first run generates the tree in
`target/benchmark/`, in about 40 seconds. A run takes about 20 minutes and writes the table to
`target/benchmark/results.md`.

## Results

The ratios are duit divided by ncdu, so below 1 is better for duit.

| Case                                       |       ncdu time |       duit time | Time ratio | ncdu peak RSS | duit peak RSS | RSS ratio |
| ------------------------------------------ | --------------: | --------------: | ---------: | ------------: | ------------: | --------: |
| Scan into memory, `-t 1`                   | 5.054 s ± 0.082 | 4.983 s ± 0.028 |       0.99 |      45.3 MiB |      34.0 MiB |      0.75 |
| Scan into memory, `-t 4`                   | 2.142 s ± 0.035 | 2.130 s ± 0.064 |       0.99 |      45.3 MiB |      38.6 MiB |      0.85 |
| Scan into memory, `-t 8`                   | 2.340 s ± 0.067 | 2.295 s ± 0.044 |       0.98 |      45.4 MiB |      39.9 MiB |      0.88 |
| Scan to JSON, `-t 1`                       | 5.015 s ± 0.021 | 4.930 s ± 0.032 |       0.98 |       2.7 MiB |       2.6 MiB |      0.98 |
| Scan to JSON, `-t 4`                       | 2.161 s ± 0.024 | 2.146 s ± 0.022 |       0.99 |      45.4 MiB |      43.2 MiB |      0.95 |
| Scan to JSON, `-t 8`                       | 2.315 s ± 0.072 | 2.330 s ± 0.053 |       1.01 |      45.5 MiB |      43.7 MiB |      0.96 |
| Scan to binary, `-t 1`                     | 4.844 s ± 0.299 | 4.936 s ± 0.074 |       1.02 |       4.0 MiB |       3.8 MiB |      0.96 |
| Scan to binary, `-t 4`                     | 2.143 s ± 0.027 | 2.122 s ± 0.034 |       0.99 |       6.0 MiB |       4.8 MiB |      0.81 |
| Scan to binary, `-t 8`                     | 2.353 s ± 0.098 | 2.227 s ± 0.080 |       0.95 |       7.5 MiB |       6.9 MiB |      0.91 |
| Import JSON into memory                    | 0.101 s ± 0.007 | 0.081 s ± 0.003 |       0.80 |      45.3 MiB |      33.8 MiB |      0.75 |
| Import binary to JSON                      | 0.080 s ± 0.002 | 0.067 s ± 0.001 |       0.84 |       3.4 MiB |       3.1 MiB |      0.91 |
| Scan `/System/Library` into memory, `-t 1` | 4.680 s ± 0.022 | 4.657 s ± 0.010 |       1.00 |      29.0 MiB |      22.3 MiB |      0.77 |
| Scan `/System/Library` into memory, `-t 8` | 1.706 s ± 0.011 | 1.794 s ± 0.012 |       1.05 |      29.3 MiB |      29.1 MiB |      0.99 |

## Gaps

M8 is not done. No row takes as much peak memory as ncdu in this table, but one is at 0.99 and has
taken more in other runs. Four rows take as much time or more: the scan of `/System/Library` with
`-t 8` (1.05) and with `-t 1` (1.00), the scan to binary with `-t 1` (1.02), and the scan to JSON
with `-t 8` (1.01). duit 0.1.2 has the Rust code of 0.1.0 with another version number and an order
file for it, so the profiles below, which are of 0.1.0, hold for it.

### More memory than ncdu

- Scan of `/System/Library` with `-t 8`: 0.99 here, 29.1 MiB against 29.3 MiB, and 0.94 in the table
  of 0.1.0. Its peak moves between runs, from 26.6 to 31.1 MiB over the runs of three builds before
  0.1.0, where ncdu's stays at 29.2 to 29.3 MiB. The heap at the end of the scan is the same as with
  `-t 1`, and the small regions of the system allocator hold memory that the threads allocated and
  the calling thread freed. A pool of the parts that the threads send did not change it.

### Slower than ncdu

- Scan of `/System/Library` with `-t 1` (1.00): duit took 0.023 s less than ncdu here, which rounds
  to 1.00, and 0.057 s more in the table of 0.1.0 (1.01). It waits on `fstatat`, once for each entry
  in both programs, and to take less time it would need fewer system calls, which on macOS means
  reading a directory with its attributes by `getattrlistbulk`, or with `getdirentries` instead of
  `readdir`. rustix 1.1.6 wraps neither on Apple platforms (its `RawDir` is for Linux only), so
  either needs `unsafe` code, which M8 rules out.
- Scan of `/System/Library` with `-t 8` (1.05, as in the table of 0.1.0). A sample of the threads
  with `-t 8` shows them in system calls, with the calling thread waiting for parts, and about 4% of
  the samples in `fstat`, `fstatfs`, and `fcntl` calls that ncdu does not make: rustix opens each
  directory for reading with libc's `fdopendir`, which makes them, where ncdu calls
  `__getdirentries64`, a private symbol of libSystem, directly. The tree has 163,817 directories, so
  this shows here and not on the generated tree, which has 1,179. As above, reading without
  `fdopendir` needs `unsafe` code.
- Scan to binary with `-t 1` (1.02) is 0.99 in a rerun, as in the table of 0.1.0. The runs of ncdu
  in the table took from 4.325 s to 5.145 s, where no other run of either program with `-t 1` on
  this tree took less than 4.885 s, so its mean of 4.844 s ± 0.299 is below what it takes otherwise.
  A rerun of 15 runs each right after the table gave 5.019 s ± 0.020 for ncdu and 4.947 s ± 0.008
  for duit.
- Scan to JSON with `-t 8` (1.01, as in the table of 0.1.0) is within the noise of the scans with
  several threads, which take 0.95 to 0.99 in the other rows of the generated tree and moved by up
  to 0.02 between the table of 0.1.0 and this one. A rerun of 15 runs each right after the table
  gave 2.344 s ± 0.042 for duit and 2.325 s ± 0.079 for ncdu, 1.01 again, with a difference of 0.019
  s that is below the ± of both. The same rerun at 0.1.0 gave 2.285 s ± 0.070 against 2.301 s ±
  0.072, so the row is at the time of ncdu and not shown below it. A `sample` of a `profiling` build
  of this row and of the scan into memory with `-t 4`, next to one of ncdu, shows no time that duit
  spends and ncdu does not. Each scan thread of both programs is in `fstatat` or `__getdirentries64`
  for 98% of its samples or more, `fstatat` alone for 75% to 95%, and duit's own code, with the
  parts that it sends, their allocation, and the queue, takes under 1%. duit's calling thread is
  parked in `recv_timeout` for 98% of its samples, and the threads have 4 samples or fewer each in
  `send`, so they never wait for it. The system time of both programs is the number of threads times
  the wall time, 8.0 to 8.5 s with `-t 4` and 16 to 17.5 s with `-t 8` for wall times of 2.1 to 2.4
  s, so each `fstatat` costs more in the kernel the more threads call it, and the wall time is the
  same in both. duit takes about 0.1 s more user time than ncdu (0.31 s against 0.21 s with `-t 4`),
  split between the calling thread, which builds the tree while the threads read, and under 1% of
  each scan thread. The JSON export after the scan does not show either: with 20 runs each and
  `-t 8`, `-o /dev/null` took 0.05 s less than `--quit-after-scan` in duit and 0.02 s more in ncdu,
  both within the ± of 0.06 to 0.09 s. As for `/System/Library`, this row would take less time only
  with fewer system calls for each entry, such as reading the attributes of a directory in bulk,
  which needs `unsafe` code.

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
- The scans to an export with one thread and the import of a binary export to JSON keep less than 1
  MiB of heap, so their peak memory is mostly the code that they keep resident. With `vmmap` during
  the scan to JSON, 496 KiB of duit's `__TEXT` was resident, against 208 KiB of ncdu's and 32 KiB of
  its `libzstd` and `libncursesw`: the 87 functions that the scan enters take 74 KB, but they were
  spread over 28 pages of 16 KiB. On macOS, `crates/duit/build.rs` now links duit with an order
  file, `crates/duit/duit.order`, which `scripts/order_file.py` (`make order`) writes from the
  functions that lldb sees each streaming case enter, grouped by the cases that enter them. The
  resident `__TEXT` of the scan to JSON went to 176 KiB, the scan to JSON with `-t 1` from 1.10 to
  0.99, the scan to binary with `-t 1` from 1.09 to 0.98, and import binary to JSON from 1.01 to
  0.92. The names in the file hold a hash of each crate, which changes with the profile, the target,
  the versions of the crates, and the toolchain, so the file lists the names of the release build
  and of a local `dist` build for the host, and needs `make order` again after such a change, such
  as the version bump of a release. Other targets, such as `x86_64-apple-darwin`, get no order. The
  linker skips the names that it does not find, so a stale file costs memory, not a failed build.
- The binary export compresses each block into the buffer that it writes, as ncdu does, instead of
  into a buffer of its own that it then copies. The scan to binary with `-t 1` peaked at 3.74 MiB
  instead of 3.81 MiB, and with `-t 4` it went from 1.00 to 0.93 with the order file.
- Measured, and left out: `fstatat` in the order of the inode numbers of a directory instead of the
  order of `readdir`, with each directory read whole first (slower with 4 threads in a separate
  program: 2.729 s ± 0.024 against 2.477 s ± 0.394, and 2.697 s against 2.101 s for the fastest
  runs), a pool of the parts that the scan threads send (no change in time or peak memory, measured
  three times), mimalloc as the global allocator (102 MiB for the scan into memory with `-t 1`,
  against 53.7 MiB at the time), reserving the whole tree up front (63 MiB against 53.7 MiB with the
  entries of 40 bytes), and `opt-level = "s"`, which kept 64 KiB less code resident before the order
  file and imported JSON 15% slower.

## Performance bugs for later

A heap or CPU profile of a build with the `profiling` profile (release with debug information and
without LTO, `cargo build --profile profiling --bin duit`) is the place to start, for example with
`sample` and `vmmap` on macOS, or `heaptrack` and `perf` on Linux.

- **The peak memory of a scan with several threads moves between runs.** See "More memory than
  ncdu".
- **`fdopendir` for each directory, and one `fstatat` for each entry.** See "Slower than ncdu".
