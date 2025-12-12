#!/usr/bin/env python3
"""Update the multipliers table in README.md with actual build counts from nix-eval-jobs."""

import json
import os
import re
import subprocess
import sys

TIERS = ["small", "medium", "large"]
KINDS = ["shallow", "deep", "mixed"]
MULTIPLIERS = [1, 2, 4, 8, 16]


def parse_package_name(attr: str) -> tuple[str, str, int] | None:
    """Parse package attr into (tier, kind, multiplier) or None if invalid."""
    # Attrs look like "small-shallow", "large-deep-4x", etc.
    match = re.match(r"^(\w+)-(\w+)(?:-(\d+)x)?$", attr)
    if not match:
        return None
    tier, kind, mult_str = match.groups()
    mult = int(mult_str) if mult_str else 1
    if tier in TIERS and kind in KINDS and mult in MULTIPLIERS:
        return (tier, kind, mult)
    return None


def get_all_build_counts() -> dict[str, dict[str, dict[int, int]]]:
    """Run nix-eval-jobs once on all packages and return build counts."""
    cmd = [
        "nix-eval-jobs",
        "--check-cache-status",
        "--impure",
        "--workers",
        str(os.cpu_count()),
        "--flake",
        ".#packages.x86_64-linux",
    ]

    print("Running nix-eval-jobs...", file=sys.stderr)
    result = subprocess.run(cmd, capture_output=True, text=True)

    # Group neededBuilds by package
    pkg_builds: dict[str, set[str]] = {}
    for line in result.stdout.strip().split("\n"):
        if not line:
            continue
        try:
            data = json.loads(line)
        except json.JSONDecodeError:
            continue

        attr = data.get("attr", "")
        needed = data.get("neededBuilds", [])

        if attr not in pkg_builds:
            pkg_builds[attr] = set()
        pkg_builds[attr].update(needed)

    # Convert to structured data
    data: dict[str, dict[str, dict[int, int]]] = {}
    for attr, builds in pkg_builds.items():
        parsed = parse_package_name(attr)
        if not parsed:
            continue
        tier, kind, mult = parsed
        count = len(builds)
        print(f"  {attr}: {count} builds", file=sys.stderr)

        if tier not in data:
            data[tier] = {}
        if kind not in data[tier]:
            data[tier][kind] = {}
        data[tier][kind][mult] = count

    return data


def generate_table(data: dict[str, dict[str, dict[int, int]]]) -> str:
    """Generate a pretty-printed, aligned markdown table from collected data."""
    # Build column headers
    mult_headers = ["Base" if m == 1 else f"{m}x" for m in MULTIPLIERS]
    headers = ["Tier", "Kind"] + mult_headers

    # Build all rows as strings
    rows: list[list[str]] = []
    for tier in TIERS:
        for kind in KINDS:
            row = [tier, kind]
            for mult in MULTIPLIERS:
                count = data.get(tier, {}).get(kind, {}).get(mult, "?")
                row.append(str(count))
            rows.append(row)

    # Calculate column widths
    widths = [len(h) for h in headers]
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(cell))

    # Format helper
    def fmt_row(cells: list[str]) -> str:
        padded = [cell.ljust(widths[i]) for i, cell in enumerate(cells)]
        return "| " + " | ".join(padded) + " |"

    def fmt_sep() -> str:
        return "|" + "|".join("-" * (w + 2) for w in widths) + "|"

    lines = [fmt_row(headers), fmt_sep()]
    lines.extend(fmt_row(row) for row in rows)

    return "\n".join(lines)


def update_readme(table: str) -> None:
    """Update the multipliers table in README.md."""
    with open("README.md") as f:
        content = f.read()

    # Find and replace the multipliers table
    # The table follows "### Multipliers" and the explanatory paragraph
    pattern = (
        r"(### Multipliers\n\n"
        r"Multipliers scale package count by duplicating packages with unique cache-busting values:\n\n)"
        r"\|[^\n]+\n"  # Header row
        r"\|[-| ]+\n"  # Separator row
        r"(?:\|[^\n]+\n)+"  # Data rows
    )

    replacement = r"\1" + table + "\n"

    new_content, count = re.subn(pattern, replacement, content)

    if count == 0:
        print("Warning: Could not find multipliers table to update", file=sys.stderr)
        return

    with open("README.md", "w") as f:
        f.write(new_content)

    print("Updated README.md")


def main() -> None:
    """Main entry point."""
    data = get_all_build_counts()

    # Generate and update table
    table = generate_table(data)
    print("\nGenerated table:", file=sys.stderr)
    print(table, file=sys.stderr)

    update_readme(table)


if __name__ == "__main__":
    main()
