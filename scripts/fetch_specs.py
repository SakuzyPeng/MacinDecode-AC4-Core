#!/usr/bin/env python3
"""按 spec/MANIFEST.json 获取并校验 ETSI 规范文件。

    ./scripts/fetch_specs.py            缺失则下载，随后校验全部哈希
    ./scripts/fetch_specs.py --verify   只校验本地文件，不访问网络
    ./scripts/fetch_specs.py --force    无条件重新下载

规范文件受 ETSI 版权保护，不进入版本控制。清单记录来源与哈希，
使任何人都能取得字节一致的副本。哈希不匹配一律视为错误：
可能是 ETSI 就地更新了文件，也可能是下载损坏，两者都需要人工判断
而非静默接受。
"""

import argparse
import hashlib
import io
import json
import ssl
import subprocess
import sys
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

SPEC_DIR = Path(__file__).resolve().parent.parent / "spec"
MANIFEST = SPEC_DIR / "MANIFEST.json"

# ETSI 对非浏览器 User-Agent 返回 403
USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0 Safari/537.36"
)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


class Failure(Exception):
    pass


def _ssl_context() -> ssl.SSLContext | None:
    """独立安装的 Python 通常没有 CA 根，可用时借用 certifi。"""
    try:
        import certifi
    except ImportError:
        return None
    return ssl.create_default_context(cafile=certifi.where())


def _download_curl(url: str, dest: Path) -> None:
    """回退路径：curl 使用系统信任库，证书校验保持开启。"""
    result = subprocess.run(
        ["curl", "-sSL", "--fail", "--max-time", "180",
         "-A", USER_AGENT, "-o", str(dest), url],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        raise Failure(f"curl 下载失败（退出码 {result.returncode}）：{result.stderr.strip()}")


def download(url: str, dest: Path) -> None:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=180, context=_ssl_context()) as resp:
            dest.write_bytes(resp.read())
    except urllib.error.HTTPError as e:
        raise Failure(f"下载失败：HTTP {e.code}") from e
    except urllib.error.URLError as e:
        if isinstance(e.reason, ssl.SSLCertVerificationError):
            _download_curl(url, dest)
            return
        raise Failure(f"下载失败：{e.reason}") from e


def ensure(entry: dict, *, verify_only: bool, force: bool) -> None:
    """确保单个文件就位且哈希匹配。"""
    path = SPEC_DIR / entry["filename"]
    expected = entry["sha256"]

    if force or not path.exists():
        if verify_only:
            raise Failure(f"{entry['filename']} 不存在（--verify 不下载）")
        print(f"  下载 {entry['filename']} …", flush=True)
        try:
            download(entry["url"], path)
        except Failure as e:
            path.unlink(missing_ok=True)  # 不留下半截文件冒充有效副本
            raise Failure(f"{entry['filename']} {e}") from e

    actual = sha256_file(path)
    if actual != expected:
        raise Failure(
            f"{entry['filename']} 哈希不匹配\n"
            f"      期望 {expected}\n"
            f"      实际 {actual}\n"
            f"      来源可能已就地更新；请核对 ETSI 发布页后再决定是否更新清单"
        )

    size = path.stat().st_size
    if "size" in entry and size != entry["size"]:
        raise Failure(f"{entry['filename']} 大小为 {size}，清单记为 {entry['size']}")


def verify_member(entry: dict) -> None:
    """校验 zip 内单个成员的内容哈希，并把它释出到 spec/。

    释出是必需的而非便利：`macindecode-ac4-bitstream` 的 build script 在构建时
    读取这些 C 表生成 Huffman 解码表。构建期不做解压，但会对实际读入字节
    再算一次 SHA-256；此处校验 zip 成员并负责安全释出。释出文件与 zip
    同样不进入版本控制。
    """
    path = SPEC_DIR / entry["filename"]
    member, expected = entry["member"], entry["member_sha256"]

    with zipfile.ZipFile(io.BytesIO(path.read_bytes())) as z:
        names = z.namelist()
        if member not in names:
            raise Failure(f"{entry['filename']} 内缺少 {member}，实际含 {names}")
        payload = z.read(member)

    actual = sha256_bytes(payload)
    if actual != expected:
        raise Failure(
            f"{entry['filename']}::{member} 哈希不匹配\n"
            f"      期望 {expected}\n      实际 {actual}"
        )

    extracted = SPEC_DIR / member
    if not extracted.exists() or sha256_file(extracted) != expected:
        extracted.write_bytes(payload)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--verify", action="store_true", help="只校验本地文件，不访问网络")
    ap.add_argument("--force", action="store_true", help="无条件重新下载")
    args = ap.parse_args()

    if args.verify and args.force:
        print("--verify 与 --force 互斥", file=sys.stderr)
        return 2

    if not MANIFEST.exists():
        print(f"缺少 {MANIFEST}", file=sys.stderr)
        return 1

    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    SPEC_DIR.mkdir(exist_ok=True)

    failures: list[str] = []

    for doc in manifest["documents"]:
        print(f"{doc['id']} v{doc['version']} ({doc['release']}) — {doc['pages']} 页")
        targets = [doc]
        if "attachment" in doc:
            targets.append(doc["attachment"])

        for entry in targets:
            try:
                ensure(entry, verify_only=args.verify, force=args.force)
                if "member" in entry:
                    verify_member(entry)
                    print(f"  {entry['filename']} ✓（已释出 {entry['member']}）")
                else:
                    print(f"  {entry['filename']} ✓")
            except Failure as e:
                print(f"  {e}")
                failures.append(entry["filename"])
        print()

    if failures:
        print(f"失败 {len(failures)} 项：{', '.join(failures)}")
        return 1

    print(f"全部就位并通过校验，基线 {manifest['baseline']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
