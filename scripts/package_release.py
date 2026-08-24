#!/usr/bin/env python3
"""Create a self-contained macinac4 release archive and SHA-256 sidecar."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import re
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
SPEC_MANIFEST = REPO_ROOT / "spec" / "MANIFEST.json"
VERSION_RE = re.compile(
    r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?"
)
TARGET_RE = re.compile(r"[0-9A-Za-z_.-]+")
COMMIT_RE = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?")


class Failure(Exception):
    pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--output-dir", required=True, type=Path)
    return parser.parse_args()


def checked_text(value: str, pattern: re.Pattern[str], label: str) -> str:
    if pattern.fullmatch(value) is None:
        raise Failure(f"invalid {label}: {value!r}")
    return value


def spec_baseline() -> str:
    try:
        manifest = json.loads(SPEC_MANIFEST.read_text(encoding="utf-8"))
        baseline = manifest["baseline"]
    except (KeyError, OSError, json.JSONDecodeError) as error:
        raise Failure(f"unable to read {SPEC_MANIFEST}: {error}") from error
    if not isinstance(baseline, str) or not baseline:
        raise Failure(f"invalid baseline in {SPEC_MANIFEST}")
    return baseline


def release_files(
    binary: Path, base_name: str, version: str, target: str, commit: str
) -> list[tuple[str, bytes, int]]:
    windows = "windows" in target
    expected_binary_name = "macinac4.exe" if windows else "macinac4"
    if not binary.is_file():
        raise Failure(f"binary does not exist: {binary}")
    if binary.name != expected_binary_name:
        raise Failure(
            f"target {target} expects {expected_binary_name}, got {binary.name}"
        )

    files: list[tuple[str, bytes, int]] = [
        (f"{base_name}/{expected_binary_name}", binary.read_bytes(), 0o755),
    ]
    for relative in (
        Path("LICENSE"),
        Path("README.md"),
        Path("README.en.md"),
        Path("Cargo.lock"),
        Path("spec/MANIFEST.json"),
    ):
        source = REPO_ROOT / relative
        try:
            payload = source.read_bytes()
        except OSError as error:
            raise Failure(f"unable to read {source}: {error}") from error
        files.append((f"{base_name}/{relative.as_posix()}", payload, 0o644))

    build_info = (
        "MacinDecode-AC4-Core release binary\n"
        f"version={version}\n"
        f"target={target}\n"
        f"commit={commit}\n"
        "features=audio-decode\n"
        f"spec_baseline={spec_baseline()}\n"
    ).encode()
    files.append((f"{base_name}/BUILD_INFO.txt", build_info, 0o644))
    return files


def write_zip(path: Path, files: list[tuple[str, bytes, int]]) -> None:
    with zipfile.ZipFile(
        path, mode="w", compression=zipfile.ZIP_DEFLATED, compresslevel=9
    ) as archive:
        for name, payload, mode in files:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = mode << 16
            archive.writestr(info, payload)


def write_tar_gz(path: Path, files: list[tuple[str, bytes, int]]) -> None:
    with path.open("wb") as raw:
        with gzip.GzipFile(
            filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0
        ) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for name, payload, mode in files:
                    info = tarfile.TarInfo(name)
                    info.size = len(payload)
                    info.mode = mode
                    info.mtime = 0
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    archive.addfile(info, io.BytesIO(payload))


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    args = parse_args()
    try:
        version = checked_text(args.version, VERSION_RE, "version")
        target = checked_text(args.target, TARGET_RE, "target")
        commit = checked_text(args.commit.lower(), COMMIT_RE, "commit")
        base_name = f"macinac4-v{version}-{target}"
        files = release_files(args.binary, base_name, version, target, commit)

        args.output_dir.mkdir(parents=True, exist_ok=True)
        suffix = ".zip" if "windows" in target else ".tar.gz"
        destination = args.output_dir / f"{base_name}{suffix}"
        with tempfile.TemporaryDirectory(dir=args.output_dir) as temporary:
            temporary_archive = Path(temporary) / destination.name
            if suffix == ".zip":
                write_zip(temporary_archive, files)
            else:
                write_tar_gz(temporary_archive, files)
            temporary_archive.replace(destination)

        checksum = args.output_dir / f"{destination.name}.sha256"
        checksum.write_text(
            f"{sha256(destination)}  {destination.name}\n", encoding="utf-8"
        )
    except (Failure, OSError) as error:
        print(f"release packaging failed: {error}", file=sys.stderr)
        return 1

    print(destination)
    print(checksum)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
