#!/usr/bin/env python3
"""Line counts for cella, per crate and per CLI.

Three sections: each crate alone; each thin CLI with every workspace
crate the CLI uses (the audit weight of one binary); and the totals.
"Source only" excludes inline `#[cfg(test)]` blocks and everything
under tests/. Used by `make lines`; kept as a standalone script so
the counting method is inspectable rather than a one-off pipeline.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
CRATES = ROOT / "crates"


def split_source_and_tests(path: Path) -> tuple[int, int]:
    """Return (source_lines, test_lines) for one file, by tracking
    brace depth from each `#[cfg(test)]` marker to the end of the
    module block the marker opens."""
    lines = path.read_text().splitlines()
    source = 0
    test = 0
    in_test_block = False
    depth = 0
    seen_brace = False
    for line in lines:
        stripped = line.strip()
        if not in_test_block and stripped.startswith("#[cfg(test)]"):
            in_test_block = True
            depth = 0
            seen_brace = False
            test += 1
            continue
        if in_test_block:
            test += 1
            depth += line.count("{") - line.count("}")
            if "{" in line:
                seen_brace = True
            if seen_brace and depth <= 0:
                in_test_block = False
            continue
        source += 1
    return source, test


def count_crate(crate: Path) -> tuple[int, int, int]:
    """(source, inline_test, tests_dir) for one crate directory."""
    source = 0
    inline = 0
    src = crate / "src"
    if src.exists():
        for p in sorted(src.rglob("*.rs")):
            s, t = split_source_and_tests(p)
            source += s
            inline += t
    tests = 0
    tdir = crate / "tests"
    if tdir.exists():
        for p in sorted(tdir.rglob("*.rs")):
            tests += len(p.read_text().splitlines())
    return source, inline, tests


def workspace_deps(crate: Path) -> list[str]:
    """The workspace crates one crate depends on: any dependency
    key named cella-* in the crate's Cargo.toml, whatever the form
    (path = ..., workspace = true, or a bare version)."""
    toml = crate / "Cargo.toml"
    if not toml.is_file():
        return []
    deps = []
    for line in toml.read_text().splitlines():
        m = re.match(r"\s*(cella-[a-z0-9-]+)\s*=", line)
        if m and (CRATES / m.group(1)).is_dir():
            deps.append(m.group(1))
    return deps


def closure(name: str, counts: dict) -> list[str]:
    """The crate and its workspace dependencies, transitively."""
    seen: list[str] = []
    stack = [name]
    while stack:
        n = stack.pop()
        if n in seen or n not in counts:
            continue
        seen.append(n)
        stack.extend(workspace_deps(CRATES / n))
    return seen


def is_cli(crate: Path) -> bool:
    """A CLI crate builds a binary: src/main.rs or src/bin/."""
    return (crate / "src" / "main.rs").is_file() or (crate / "src" / "bin").is_dir()


def main() -> None:
    if not CRATES.is_dir():
        print("no crates/ directory -- the pre-split layout is gone; nothing to count")
        return 1

    crates = sorted(p for p in CRATES.iterdir() if p.is_dir())
    counts = {}
    for c in crates:
        counts[c.name] = count_crate(c)

    header = f"  {'crate':<20} {'source':>7} {'source + test':>14}"
    print("PER CRATE")
    print(header)
    print("  " + "-" * 43)
    for name, (s, i, t) in sorted(counts.items()):
        print(f"  {name:<20} {s:>7} {s + i + t:>14}")
    print()

    print("PER CLI (the binary's audit weight: the crate plus every workspace crate the CLI uses)")
    print(f"  {'cli':<20} {'source':>7} {'source + test':>14}  {'depends on':<}")
    print("  " + "-" * 60)
    for c in crates:
        if not is_cli(c):
            continue
        members = closure(c.name, counts)
        s = sum(counts[m][0] for m in members)
        i = sum(counts[m][1] for m in members)
        t = sum(counts[m][2] for m in members)
        others = sorted(m for m in members if m != c.name)
        uses = ", ".join(others) if others else "-"
        print(f"  {c.name:<20} {s:>7} {s + i + t:>14}  {uses}")
    print()

    total_s = sum(v[0] for v in counts.values())
    total_i = sum(v[1] for v in counts.values())
    total_t = sum(v[2] for v in counts.values())
    print("-" * 62)
    print(f"{'SOURCE ONLY (all crates)':<40} {total_s:>6}")
    print(f"{'SOURCE + ALL TESTS (inline + tests/)':<40} {total_s + total_i + total_t:>6}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
