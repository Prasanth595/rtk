#!/usr/bin/env python3
"""Update the existing MacPorts port from verified stable release assets."""

import argparse
import hashlib
from pathlib import Path
import re


TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin")
VERSION = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"


def stable_version(tag):
    if not re.fullmatch("v" + VERSION, tag):
        raise ValueError(f"Expected a stable vMAJOR.MINOR.PATCH tag, got {tag!r}")
    return tuple(int(part) for part in tag[1:].split("."))


def archive_checksums(assets, target):
    name = f"rtk-{target}.tar.gz"
    entries = [
        fields[0].lower()
        for line in (assets / "checksums.txt").read_text().splitlines()
        if len(fields := line.split()) == 2 and fields[1].lstrip("*") == name
    ]
    if len(entries) != 1 or not re.fullmatch(r"[a-f0-9]{64}", entries[0]):
        raise ValueError(f"Expected exactly one SHA-256 entry for {name}")
    data = (assets / name).read_bytes()
    sha256 = hashlib.sha256(data).hexdigest()
    if sha256 != entries[0]:
        raise ValueError(f"Release checksum mismatch for {name}")
    return {
        "rmd160": hashlib.new("ripemd160", data).hexdigest(),
        "sha256": sha256,
        "size": str(len(data)),
    }


def replace_once(pattern, replacement, text):
    updated, count = re.subn(pattern, replacement, text, flags=re.MULTILINE)
    if count != 1:
        raise ValueError(f"Unexpected Portfile layout: {pattern!r} matched {count} times")
    return updated


def update_architecture(text, target, checksums):
    pattern = (
        r"^[ \t]*distname[ \t]+rtk-" + re.escape(target)
        + r"[ \t]*\n[ \t]*checksums(?:[^\n\\]|\\\n)*"
    )

    def replace_block(match):
        block = match.group()
        for key, value in checksums.items():
            block = replace_once(
                r"\b(" + key + r"[ \t]+)[^\s\\]+",
                lambda field: field[1] + value,
                block,
            )
        return block

    return replace_once(pattern, replace_block, text)


def update_portfile(portfile, assets, tag):
    incoming = stable_version(tag)
    original = portfile.read_text()
    versions = re.findall(r"^version[ \t]+(\S+)[ \t]*$", original, re.MULTILINE)
    if len(versions) != 1:
        raise ValueError("Expected exactly one literal version in the Portfile")
    current = stable_version("v" + versions[0])
    if incoming < current:
        raise ValueError(f"Refusing to downgrade {versions[0]} to {tag[1:]}")

    updated = original
    for target in TARGETS:
        updated = update_architecture(updated, target, archive_checksums(assets, target))
    if incoming == current:
        if updated != original:
            raise ValueError("Checksums changed for an existing version; review manually")
        return False

    updated = replace_once(
        r"^(version[ \t]+)\S+",
        lambda match: match[1] + tag[1:],
        updated,
    )
    updated = re.sub(r"^(revision[ \t]+)\d+", r"\g<1>0", updated, flags=re.MULTILINE)
    portfile.write_text(updated)
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--portfile", type=Path, required=True)
    parser.add_argument("--assets", type=Path, required=True)
    args = parser.parse_args()
    try:
        changed = update_portfile(args.portfile, args.assets, args.tag)
    except (ValueError, OSError) as error:
        parser.exit(1, f"MacPorts update failed: {error}\n")
    print("Updated Portfile" if changed else "Portfile is already up to date")


if __name__ == "__main__":
    main()
