#!/usr/bin/env -S uvx --python ~=3.14.0 python
"""Compare the scan time and the peak memory of duit with those of ncdu 2.9.2.

Builds duit with the release profile, generates a deterministic tree of about one million entries
in `target/benchmark/`, and runs each case with both programs and the same flags: `hyperfine` for
the time, and `/usr/bin/time` for the peak resident set size. Writes the results to
`target/benchmark/results.md` as a Markdown table, the one in `BENCHMARKS.md`.

Usage: `make bench` or `uvx --python '~=3.14.0' python scripts/benchmark.py --help`. Needs uv,
`hyperfine`, ncdu 2.9.2, and `/usr/bin/time`, and only the standard library of Python 3.14.
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
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import Iterator, Sequence

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
# Every tenth file of a leaf directory is a hard link to the file before it.
HARD_LINK_PERIOD = 10
LONG_NAMES = 10_000
DEEP_LEVELS = 64
DEEP_FILES = 10
NON_EMPTY_FILE_PERIOD = 16
SHORTEST_LONG_NAME = 100
LONGEST_NAME = 255


def create_file(directory_fd: int, name: str, index: int) -> None:
    fd = os.open(name, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o644, dir_fd=directory_fd)
    if index % NON_EMPTY_FILE_PERIOD == 0:
        os.write(fd, b"x" * (index % 1000 + 1))
    os.close(fd)


def make_directory(parent_fd: int, name: str) -> int:
    os.mkdir(name, 0o755, dir_fd=parent_fd)
    return os.open(name, os.O_RDONLY | os.O_DIRECTORY, dir_fd=parent_fd)


@dataclass
class Leaves:
    """The files that the leaf directories hold so far, and a descriptor of the last one."""

    file_count: int = 0
    previous_fd: int | None = None


def generate_nested(parent_fd: int, level: int, leaves: Leaves) -> None:
    for child in range(FANOUT):
        fd = make_directory(parent_fd, f"directory-{child}")
        if level + 1 < NESTED_LEVELS:
            generate_nested(fd, level + 1, leaves)
        else:
            for index in range(LEAF_FILES):
                name = f"file-{index:04d}"
                if index % HARD_LINK_PERIOD == HARD_LINK_PERIOD - 1:
                    os.link(f"file-{index - 1:04d}", name, src_dir_fd=fd, dst_dir_fd=fd)
                else:
                    create_file(fd, name, leaves.file_count + index)
            if leaves.previous_fd is not None:
                os.link(
                    "file-0000",
                    "link-to-previous-leaf",
                    src_dir_fd=leaves.previous_fd,
                    dst_dir_fd=fd,
                )
                os.close(leaves.previous_fd)
            leaves.previous_fd = os.dup(fd)
            leaves.file_count += LEAF_FILES
        os.close(fd)


def generate() -> None:
    if TREE.exists():
        shutil.rmtree(TREE)
    TREE.mkdir(parents=True)
    root_fd = os.open(TREE, os.O_RDONLY | os.O_DIRECTORY)

    fd = make_directory(root_fd, "wide")
    for index in range(WIDE_FILES):
        create_file(fd, f"file-{index:06d}", index)
    os.close(fd)

    fd = make_directory(root_fd, "nested")
    leaves = Leaves()
    generate_nested(fd, 0, leaves)
    if leaves.previous_fd is not None:
        os.close(leaves.previous_fd)
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


def stamp() -> str:
    generator = "".join(
        inspect.getsource(function)
        for function in (create_file, make_directory, Leaves, generate_nested, generate)
    )
    return json.dumps(
        [
            hashlib.sha256(generator.encode()).hexdigest(),
            WIDE_FILES,
            FANOUT,
            NESTED_LEVELS,
            LEAF_FILES,
            HARD_LINK_PERIOD,
            LONG_NAMES,
            DEEP_LEVELS,
            DEEP_FILES,
            NON_EMPTY_FILE_PERIOD,
            SHORTEST_LONG_NAME,
            LONGEST_NAME,
        ]
    )


def prepare_tree() -> None:
    WORK.mkdir(parents=True, exist_ok=True)
    if not STAMP.exists() or STAMP.read_text() != stamp():
        print(f"Generating {TREE}", file=sys.stderr)
        STAMP.unlink(missing_ok=True)
        generate()
        STAMP.write_text(stamp())


def run(command: Sequence[object], **kwargs: Any) -> subprocess.CompletedProcess[Any]:
    print("+ " + shlex.join(str(part) for part in command), file=sys.stderr)
    return subprocess.run([str(part) for part in command], check=True, **kwargs)


def peak_memory(command: list[str], runs: int) -> float:
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


def export_entries(path: Path) -> list[Any]:
    with path.open() as file:
        return json.load(file)[3:]


def cases(threads: list[int], real_tree: str) -> Iterator[tuple[str, list[str]]]:
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


def count_entries(item: object) -> int:
    if isinstance(item, list):
        return 1 + sum(count_entries(child) for child in item[1:])
    return 1


def mebibytes(size: float) -> str:
    return f"{size / 2**20:.1f} MiB"


def main() -> None:
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
        check=False,
    ).stdout.strip()
    duit_version = run([DUIT, "--version"], capture_output=True, text=True).stdout.strip()
    hyperfine_version = run(["hyperfine", "--version"], capture_output=True, text=True).stdout
    lines = [
        f"- Machine: {machine}, {os.cpu_count()} logical CPUs",
        f"- OS: {platform.system()} {platform.release()} ({platform.platform()})",
        f"- Versions: {version}, {duit_version}, {hyperfine_version.strip()}",
        f"- Tree: {count_entries(entries[0])} entries",
        (
            f"- Runs: {arguments.warmup} warmup and {arguments.runs} timed runs per command, peak"
            f" memory is the median of {arguments.memory_runs} runs"
        ),
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
