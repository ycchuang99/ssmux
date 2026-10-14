#!/usr/bin/env python3
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
VERSION_PATTERN = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")


def parse_version(value):
    match = VERSION_PATTERN.fullmatch(value)
    if not match:
        raise ValueError(f"expected a stable semantic version, got {value!r}")
    return tuple(map(int, match.groups()))


def format_version(parts):
    return ".".join(map(str, parts))


def package_version():
    cargo_toml = (ROOT / "Cargo.toml").read_text()
    package = re.search(r"(?ms)^\[package\]\s*(.*?)(?=^\[|\Z)", cargo_toml)
    match = re.search(r'(?m)^version = "([^"]+)"$', package.group(1)) if package else None
    if not match:
        raise ValueError("could not read [package].version from Cargo.toml")
    parse_version(match.group(1))
    return match.group(1)


def latest_tag():
    tags = subprocess.check_output(["git", "tag", "--list", "v*"], text=True).splitlines()
    versions = [(parse_version(tag[1:]), tag) for tag in tags if VERSION_PATTERN.fullmatch(tag[1:])]
    return max(versions, default=((0, 0, 0), None))


def commit_bump(message):
    header, _, body = message.partition("\n")
    if re.search(r"(?im)^BREAKING(?: CHANGE|-CHANGE)(?:[-\w]*)?:\s", body):
        return "major"
    match = re.match(r"^([a-zA-Z][\w-]*)(?:\([^)]*\))?(!)?:\s", header)
    if not match:
        return None
    if match.group(2):
        return "major"
    kind = match.group(1).lower()
    if kind == "feat":
        return "minor"
    if kind in {"fix", "perf", "revert"}:
        return "patch"
    return None


def bump_level(messages):
    levels = {"patch": 1, "minor": 2, "major": 3}
    changes = [commit_bump(message) for message in messages]
    changes = [change for change in changes if change]
    return max(changes, key=levels.get) if changes else None


def next_version():
    current = package_version()
    current_parts = parse_version(current)
    latest_parts, tag = latest_tag()

    # A manually prepared version ahead of the latest tag is an unreleased version.
    if current_parts > latest_parts:
        return current

    revision = f"{tag}..HEAD" if tag else "HEAD"
    log = subprocess.check_output(
        ["git", "log", "--no-merges", "--format=%s%n%b%x00", revision], text=True
    )
    level = bump_level([item.strip() for item in log.split("\0") if item.strip()])
    if not level:
        return ""

    major, minor, patch = max(current_parts, latest_parts)
    if level == "major":
        return format_version((major + 1, 0, 0))
    if level == "minor":
        return format_version((major, minor + 1, 0))
    return format_version((major, minor, patch + 1))


def set_version(version):
    parse_version(version)
    cargo_toml_path = ROOT / "Cargo.toml"
    cargo_toml = cargo_toml_path.read_text()
    package = re.search(r"(?ms)^\[package\]\s*(.*?)(?=^\[|\Z)", cargo_toml)
    if not package:
        raise ValueError("could not find [package] in Cargo.toml")
    package_text = package.group(1)
    package_text, count = re.subn(
        r'(?m)^version = "[^"]+"$', f'version = "{version}"', package_text, count=1
    )
    if count != 1:
        raise ValueError("could not update [package].version in Cargo.toml")
    updated_cargo_toml = (
        cargo_toml[: package.start(1)] + package_text + cargo_toml[package.end(1) :]
    )

    lock_path = ROOT / "Cargo.lock"
    lock = lock_path.read_text()
    packages = re.split(r"(?m)(?=^\[\[package\]\]$)", lock)
    matches = [
        i
        for i, package_text in enumerate(packages)
        if re.search(r'(?m)^name = "ssmux"$', package_text)
    ]
    if len(matches) != 1:
        raise ValueError("expected exactly one ssmux package entry in Cargo.lock")
    index = matches[0]
    packages[index], count = re.subn(
        r'(?m)^version = "[^"]+"$', f'version = "{version}"', packages[index], count=1
    )
    if count != 1:
        raise ValueError("could not update ssmux version in Cargo.lock")
    updated_lock = "".join(packages)
    cargo_toml_path.write_text(updated_cargo_toml)
    lock_path.write_text(updated_lock)


def self_test():
    assert parse_version("1.2.3") == (1, 2, 3)
    try:
        parse_version("01.2.3")
    except ValueError:
        pass
    else:
        raise AssertionError("leading zeroes are not valid in semantic versions")
    assert commit_bump("feat(ui): add menu") == "minor"
    assert commit_bump("fix: repair startup") == "patch"
    assert commit_bump("feat!: change config format") == "major"
    assert commit_bump("docs: clarify setup") is None
    assert commit_bump("feat: change API\n\nBREAKING CHANGE: old keys removed") == "major"
    assert commit_bump("docs: change API\n\nBREAKING-CHANGE: old keys removed") == "major"
    assert format_version((2, 0, 0)) == "2.0.0"


def main():
    if sys.argv[1:] == ["next"]:
        print(next_version())
    elif len(sys.argv) == 3 and sys.argv[1] == "set":
        set_version(sys.argv[2])
    elif sys.argv[1:] == ["self-test"]:
        self_test()
    else:
        raise SystemExit("usage: release-version.py next | set VERSION | self-test")


if __name__ == "__main__":
    main()
