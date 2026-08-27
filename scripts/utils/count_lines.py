#!/usr/bin/env python3
"""Line counts for cella: source-only vs. source+tests.

"Source only" excludes both inline `#[cfg(test)] mod tests { ... }`
blocks inside src/ and everything under tests/. "Source + tests" is
everything. Used by `make lines`; kept as a standalone script so the
counting method is inspectable rather than a one-off shell pipeline.
"""
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent


def split_source_and_tests(path: Path) -> tuple[int, int]:
    """Return (source_lines, test_lines) for one file, by tracking
    brace depth from the first `#[cfg(test)]` marker to find where its
    module block ends."""
    lines = path.read_text().splitlines()
    source = 0
    test = 0
    in_test_block = False
    depth = 0
    for line in lines:
        stripped = line.strip()
        if not in_test_block and stripped.startswith("#[cfg(test)]"):
            in_test_block = True
            depth = 0
            test += 1
            continue
        if in_test_block:
            test += 1
            depth += line.count("{") - line.count("}")
            if depth <= 0 and "{" in "".join(lines[: lines.index(line) + 1]):
                # Once we've seen at least one brace and depth returns
                # to 0, the `mod tests { ... }` block (and the
                # #[cfg(test)] line before it) is done.
                if "{" in line or depth < 0 or (depth == 0 and "}" in line):
                    in_test_block = False
            continue
        source += 1
    return source, test


def count_dir(rel: str) -> tuple[int, int]:
    total_source = 0
    total_test = 0
    for p in sorted((ROOT / rel).rglob("*.rs")):
        s, t = split_source_and_tests(p)
        total_source += s
        total_test += t
    return total_source, total_test


def count_plain(rel: str) -> int:
    total = 0
    d = ROOT / rel
    if not d.exists():
        return 0
    for p in sorted(d.rglob("*.rs")):
        total += len(p.read_text().splitlines())
    return total


def main() -> None:
    src_source, src_inline_test = count_dir("src")
    tests_dir_total = count_plain("tests")

    print(f"{'src/ (excluding inline #[cfg(test)] blocks)':<55} {src_source:>6}")
    print(f"{'src/ inline #[cfg(test)] test modules':<55} {src_inline_test:>6}")
    print(f"{'tests/ (integration tests)':<55} {tests_dir_total:>6}")
    print("-" * 62)
    print(f"{'SOURCE ONLY':<55} {src_source:>6}")
    print(f"{'SOURCE + ALL TESTS (inline + tests/)':<55} "
          f"{src_source + src_inline_test + tests_dir_total:>6}")


if __name__ == "__main__":
    sys.exit(main())
