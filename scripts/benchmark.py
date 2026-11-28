#!/usr/bin/env python3
"""Compare the scan time and the peak memory of duit with those of ncdu 2.9.2.

Builds duit with the release profile, generates a deterministic tree of about one million entries
in `target/benchmark/`, and runs each case with both programs and the same flags: `hyperfine` for
the time, and `/usr/bin/time` for the peak resident set size. Writes the results to
`target/benchmark/results.md` as a Markdown table, the one in `BENCHMARKS.md`.

Usage: `make bench` or `python3 scripts/benchmark.py --help`. Needs only the Python standard
library, `hyperfine`, ncdu 2.9.2, and `/usr/bin/time`.
"""

import argparse
import hashlib
import inspect
import json
import os
import platform
import re
import shlex
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "target" / "benchmark"
# The `.noindex` suffix keeps Spotlight from indexing the tree while the benchmarks run.
TREE = WORK / "tree.noindex"
STAMP = WORK / "tree.stamp"
DUIT = ROOT / "target" / "release" / "duit"

WIDE_FILES = 100_000
FANOUT = 10
NESTED_LEVELS = 3
LEAF_FILES = 880
LONG_NAMES = 10_000
DEEP_LEVELS = 64
DEEP_FILES = 10
NON_EMPTY_FILE_PERIOD = 16
SHORTEST_LONG_NAME = 100
LONGEST_NAME = 255


def create_file(directory_fd, name, index):
    fd = os.open(name, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o644, dir_fd=directory_fd)
    if index % NON_EMPTY_FILE_PERIOD == 0:
        os.write(fd, b"x" * (index % 1000 + 1))
    os.close(fd)


def make_directory(parent_fd, name):
    os.mkdir(name, 0o755, dir_fd=parent_fd)
    return os.open(name, os.O_RDONLY | os.O_DIRECTORY, dir_fd=parent_fd)


def generate_nested(parent_fd, level, counter):
    for child in range(FANOUT):
        fd = make_directory(parent_fd, f"directory-{child}")
        if level + 1 < NESTED_LEVELS:
            generate_nested(fd, level + 1, counter)
        else:
            for index in range(LEAF_FILES):
                name = f"file-{index:04d}"
                if index % 10 == 9:
                    os.link(f"file-{index - 1:04d}", name, src_dir_fd=fd, dst_dir_fd=fd)
                else:
                    create_file(fd, name, counter[0] + index)
            if counter[1] is not None:
                os.link("file-0000", "link-to-previous-leaf", src_dir_fd=counter[1], dst_dir_fd=fd)
                os.close(counter[1])
            counter[1] = os.dup(fd)
            counter[0] += LEAF_FILES
        os.close(fd)


def generate():
    if TREE.exists():
        shutil.rmtree(TREE)
    TREE.mkdir(parents=True)
    root_fd = os.open(TREE, os.O_RDONLY | os.O_DIRECTORY)

    fd = make_directory(root_fd, "wide")
    for index in range(WIDE_FILES):
        create_file(fd, f"file-{index:06d}", index)
    os.close(fd)

    fd = make_directory(root_fd, "nested")
    counter = [0, None]
    generate_nested(fd, 0, counter)
    os.close(counter[1])
    os.close(fd)

    fd = make_directory(root_fd, "long")
    for index in range(LONG_NAMES):
        prefix = f"{index:05d}-"
        length = SHORTEST_LONG_NAME + index % (LONGEST_NAME - SHORTEST_LONG_NAME + 1)
        create_file(fd, prefix + "n" * (length - len(prefix)), index)
    os.close(fd)

    fd = make_directory(root_fd, "deep")
    for level in range(DEEP_LEVELS):
        for index in range(DEEP_FILES):
            create_file(fd, f"file-{index}", level * DEEP_FILES + index)
        child_fd = make_directory(fd, f"level-{level:02d}")
        os.close(fd)
        fd = child_fd
    os.close(fd)
    os.close(root_fd)


def stamp():
    generator = "".join(
        inspect.getsource(function)
        for function in (create_file, make_directory, generate_nested, generate)
    )
    return json.dumps(
        [hashlib.sha256(generator.encode()).hexdigest(), WIDE_FILES, FANOUT, NESTED_LEVELS,
         LEAF_FILES, LONG_NAMES, DEEP_LEVELS, DEEP_FILES, NON_EMPTY_FILE_PERIOD,
         SHORTEST_LONG_NAME, LONGEST_NAME]
    )


def prepare_tree():
    WORK.mkdir(parents=True, exist_ok=True)
    if not STAMP.exists() or STAMP.read_text() != stamp():
        print(f"Generating {TREE}", file=sys.stderr)
        STAMP.unlink(missing_ok=True)
        generate()
        STAMP.write_text(stamp())


def run(command, **kwargs):
    print("+ " + shlex.join(str(part) for part in command), file=sys.stderr)
    return subprocess.run([str(part) for part in command], check=True, **kwargs)


def peak_memory(command, runs):
    if sys.platform == "darwin":
        time_command, pattern, unit = ["-l"], r"(\d+)\s+maximum resident set size", 1
    else:
        time_command, pattern, unit = ["-v"], r"Maximum resident set size \(kbytes\): (\d+)", 1024
    samples = []
    for _ in range(runs):
        result = subprocess.run(
            ["/usr/bin/time", *time_command, *command],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            check=True,
        )
        match = re.search(pattern, result.stderr)
        if match is None:
            sys.exit(f"No peak memory in the output of /usr/bin/time:\n{result.stderr}")
        samples.append(int(match.group(1)) * unit)
    return statistics.median(samples)


def export_entries(path):
    with open(path) as file:
        return json.load(file)[3:]


def cases(threads, real_tree):
    tree = str(TREE)
    for count in threads:
        yield f"Scan into memory, `-t {count}`", ["-t", str(count), "--quit-after-scan", tree]
    for count in threads:
        yield f"Scan to JSON, `-t {count}`", ["-t", str(count), "-o", "/dev/null", tree]
    for count in threads:
        yield f"Scan to binary, `-t {count}`", ["-t", str(count), "-O", "/dev/null", tree]
    yield "Import JSON into memory", ["-f", str(WORK / "tree.json"), "--quit-after-scan"]
    # `--quit-after-scan` returns before the browser reads a binary export, so convert it instead.
    yield "Import binary to JSON", ["-f", str(WORK / "tree.ncdu"), "-o", "/dev/null"]
    for count in (threads[0], threads[-1]):
        yield (
            f"Scan `{real_tree}` into memory, `-t {count}`",
            ["-x", "-t", str(count), "--quit-after-scan", real_tree],
        )


def count_entries(item):
    if isinstance(item, list):
        return 1 + sum(count_entries(child) for child in item[1:])
    return 1



def mebibytes(size):
    return f"{size / 2**20:.1f} MiB"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--ncdu", default=shutil.which("ncdu") or "ncdu", help="the ncdu binary")
    parser.add_argument("--runs", type=int, default=10, help="timed runs of each command")
    parser.add_argument("--warmup", type=int, default=3, help="untimed runs before them")
    parser.add_argument("--memory-runs", type=int, default=5, help="runs to measure memory")
    parser.add_argument(
        "--threads",
        type=lambda value: [int(count) for count in value.split(",")],
        default=[1, 4, 8],
        help="comma-separated values of -t, default 1,4,8",
    )
    parser.add_argument(
        "--real-tree",
        default="/System/Library" if sys.platform == "darwin" else "/usr",
        help="a real directory to scan, read-only",
    )
    arguments = parser.parse_args()

    version = subprocess.run(
        [arguments.ncdu, "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()
    if version != "ncdu 2.9.2":
        sys.exit(f"ncdu 2.9.2 is required, found: {version}")
    if shutil.which("hyperfine") is None:
        sys.exit("hyperfine is required: https://github.com/sharkdp/hyperfine")

    run(["cargo", "build", "--release", "--locked", "--bin", "duit"], cwd=ROOT)
    prepare_tree()

    programs = {"ncdu": [arguments.ncdu], "duit": [str(DUIT)]}
    common = ["--ignore-config", "-0"]

    run([arguments.ncdu, *common, "-o", WORK / "tree.json", TREE])
    run([arguments.ncdu, *common, "-O", WORK / "tree.ncdu", TREE])
    run([DUIT, *common, "-o", WORK / "duit.json", TREE])
    entries = export_entries(WORK / "tree.json")
    if export_entries(WORK / "duit.json") != entries:
        sys.exit("duit and ncdu wrote different exports of the tree")

    rows = []
    for name, flags in cases(arguments.threads, arguments.real_tree):
        print(f"\n{name}", file=sys.stderr)
        output = WORK / "hyperfine.json"
        command = ["hyperfine", "-N", "--warmup", arguments.warmup, "--runs", arguments.runs]
        command += ["--export-json", output]
        for program, binary in programs.items():
            command += ["-n", program, shlex.join([*binary, *common, *flags])]
        run(command)
        times = {
            result.get("name", result["command"]): result.get("summary", {}).get(
                "time_wall_clock", result
            )
            for result in json.loads(output.read_text())["results"]
        }
        memory = {
            program: peak_memory([*binary, *common, *flags], arguments.memory_runs)
            for program, binary in programs.items()
        }
        rows.append((name, times, memory))

    machine = subprocess.run(
        ["sysctl", "-n", "machdep.cpu.brand_string"]
        if sys.platform == "darwin"
        else ["sh", "-c", "grep -m1 'model name' /proc/cpuinfo | cut -d: -f2"],
        capture_output=True,
        text=True,
    ).stdout.strip()
    duit_version = run([DUIT, "--version"], capture_output=True, text=True).stdout.strip()
    hyperfine_version = run(["hyperfine", "--version"], capture_output=True, text=True).stdout
    lines = [
        f"- Machine: {machine}, {os.cpu_count()} logical CPUs",
        f"- OS: {platform.system()} {platform.release()} ({platform.platform()})",
        f"- Versions: {version}, {duit_version}, {hyperfine_version.strip()}",
        f"- Tree: {count_entries(entries[0])} entries",
        f"- Runs: {arguments.warmup} warmup and {arguments.runs} timed runs per command, peak"
        f" memory is the median of {arguments.memory_runs} runs",
        "",
        "| Case | ncdu time | duit time | Time ratio | ncdu peak RSS | duit peak RSS | RSS ratio |",
        "| --- | --: | --: | --: | --: | --: | --: |",
    ]
    for name, times, memory in rows:
        ncdu, duit = times["ncdu"], times["duit"]
        lines.append(
            f"| {name} | {ncdu['mean']:.3f} s ± {ncdu['stddev']:.3f} |"
            f" {duit['mean']:.3f} s ± {duit['stddev']:.3f} | {duit['mean'] / ncdu['mean']:.2f} |"
            f" {mebibytes(memory['ncdu'])} | {mebibytes(memory['duit'])} |"
            f" {memory['duit'] / memory['ncdu']:.2f} |"
        )
    (WORK / "results.md").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))

if __name__ == "__main__":
    main()
