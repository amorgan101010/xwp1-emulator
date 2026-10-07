#!/usr/bin/env python3
"""Export the reviewed source files into a separate public release tree."""

import argparse
import gzip
import hashlib
import os
import re
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def crate_version(crate):
    return re.search(r'^version = "([^"]+)"', (ROOT / crate / "Cargo.toml").read_text(), re.M).group(1)


# One number for the three crates: the release is named after it.
VERSION = crate_version("xwp1")
if {crate_version(c) for c in ("xwp1-app", "xwp1-plugin")} != {VERSION}:
    raise SystemExit("xwp1, xwp1-app and xwp1-plugin must have the same version")
MANIFEST = ROOT / "release-manifest.txt"
FORBIDDEN = ("firmware/", "out/", "rec/", "hw/", "emu/", "ref/", ".wolf/", ".claude/")
FORBIDDEN_SUFFIXES = {".zip", ".pdf", ".bin", ".zal"}
# Paths of whoever runs the export must not appear in it (taken from the environment, so no name is written here).
_USER = Path.home().name.encode()
PRIVATE_MARKERS = (b"/home/" + _USER + b"/", b"/Users/" + _USER + b"/", b"~/" + b"Repositories/")


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def files():
    names = [line.strip() for line in MANIFEST.read_text().splitlines()
             if line.strip() and not line.startswith("#")]
    if names != sorted(set(names)):
        raise SystemExit("release manifest must be sorted with no duplicate paths")
    for name in names:
        relative = Path(name)
        if (relative.is_absolute() or ".." in relative.parts or
                any(name.startswith(prefix) for prefix in FORBIDDEN) or
                relative.suffix.lower() in FORBIDDEN_SUFFIXES):
            raise SystemExit(f"forbidden release path: {name}")
        source = ROOT / relative
        if not source.is_file() or source.is_symlink():
            raise SystemExit(f"missing or linked release file: {name}")
        if source.stat().st_size > 10 * 1024 * 1024:
            raise SystemExit(f"unexpectedly large release file: {name}")
        if source.suffix.lower() in {".rs", ".js", ".cjs", ".json", ".md", ".txt", ".toml", ".sh", ".py", ".html", ".css", ".svg"}:
            data = source.read_bytes()
            if any(marker in data for marker in PRIVATE_MARKERS):
                raise SystemExit(f"private development path in: {name}")
        yield name, source


def export(destination):
    reviewed = list(files())
    if not (ROOT / ".git").is_dir():
        raise SystemExit("private source tree has no Git history; commit it before exporting")
    status = subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True)
    if status.strip():
        raise SystemExit("private source tree has uncommitted files; commit them before exporting")
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if destination.exists():
        raise SystemExit(f"release destination already exists: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="xwp1-export-", dir=destination.parent) as temp:
        tree = Path(temp) / f"xwp1-{VERSION}"
        tree.mkdir()
        (tree / "SOURCE_COMMIT").write_text(commit + "\n")
        sums = [f"{digest(tree / 'SOURCE_COMMIT')}  SOURCE_COMMIT\n"]
        for name, source in reviewed:
            target = tree / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            os.chmod(target, 0o755 if source.stat().st_mode & 0o111 else 0o644)
            if digest(source) != digest(target):
                raise SystemExit(f"copy verification failed: {name}")
            sums.append(f"{digest(target)}  {name}\n")
        (tree / "SHA256SUMS").write_text("".join(sums))
        shutil.move(tree, destination)
    archive = destination.parent / f"xwp1-{VERSION}.tar.gz"
    if archive.exists():
        raise SystemExit(f"release archive already exists: {archive}")
    with archive.open("wb") as raw:
        with gzip.GzipFile(fileobj=raw, mode="wb", filename="", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as tar:
                for path in sorted(destination.rglob("*")):
                    if not path.is_file():
                        continue
                    info = tar.gettarinfo(str(path), arcname=str(path.relative_to(destination.parent)))
                    info.mtime = 0
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    with path.open("rb") as stream:
                        tar.addfile(info, stream)
    (archive.parent / f"{archive.name}.sha256").write_text(f"{digest(archive)}  {archive.name}\n")
    print(f"exported {len(reviewed)} reviewed files from {commit} to {destination}")
    print(f"archive: {archive}")
    print(f"sha256: {digest(archive)}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="validate the reviewed file list without exporting")
    parser.add_argument("--output", type=Path, default=ROOT / "dist" / f"xwp1-{VERSION}")
    args = parser.parse_args()
    reviewed = list(files())
    print(f"validated {len(reviewed)} reviewed release files")
    if not args.check:
        export(args.output.resolve())


if __name__ == "__main__":
    main()
