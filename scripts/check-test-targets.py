#!/usr/bin/env python3
import glob
import json
import os
import re
import subprocess
import sys

LIMIT = 14


def main():
    metadata = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    data = json.loads(metadata)
    targets = [t for pkg in data["packages"] for t in pkg["targets"] if "test" in t["kind"]]
    names = sorted(t["name"] for t in targets)
    if len(names) > LIMIT:
        sys.exit("test targets grew to %d (limit %d): %s" % (len(names), LIMIT, names))
    print("test targets: %d <= %d: %s" % (len(names), LIMIT, names))

    declared = {os.path.realpath(t["src_path"]) for t in targets}
    orphans = sorted(p for p in glob.glob("tests/*.rs") if os.path.realpath(p) not in declared)
    if orphans:
        sys.exit("autotests = false: these tests/*.rs are no [[test]] target and never compile: %s" % orphans)

    with open("tests/it/main.rs") as f:
        mods = set(re.findall(r"^[ \t]*mod[ \t]+(\w+)[ \t]*;", f.read(), re.M))
    files = sorted(os.path.splitext(os.path.basename(p))[0] for p in glob.glob("tests/it/*.rs"))
    unlinked = [n for n in files if n != "main" and n not in mods]
    if unlinked:
        sys.exit("tests/it/*.rs with no mod line in tests/it/main.rs never compile: %s" % unlinked)
    print("every tests/*.rs is a [[test]] target and every tests/it/*.rs is a mod of tests/it/main.rs")


if __name__ == "__main__":
    main()
