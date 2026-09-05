#!/usr/bin/env python3
"""
SmartZip 测试夹具生成器 (Python 库版)

使用纯 Python 库创建所有测试夹具，不依赖 7z CLI 的参数解析。
- pyzipper: 创建加密 ZIP (AES-256 / ZipCrypto)
- py7zr:  创建加密 7z
- tarfile + gzip/bz2: 创建 tar.gz / tar.bz2
- ffmpeg (可选): 创建视频测试文件

用法:
    uv run generate.py [--force]
"""

import os
import sys
import shutil
import argparse
import time as _time
import io
from pathlib import Path

# ── Python 库 ──────────────────────────────────────────────────────
import zipfile as _zipfile
import pyzipper
import py7zr
import tarfile
import gzip

# FIXTURES_DIR = Path(__file__).parent.resolve()
# 兼容直接 python3 运行或 uv run 运行
FIXTURES_DIR = Path.cwd()
WORK_DIR = FIXTURES_DIR / ".work"


def run_cmd(*args, **kwargs):
    """运行外部命令 (仅用于 ffmpeg / rar)."""
    import subprocess
    if "stdout" not in kwargs and "stderr" not in kwargs:
        kwargs.setdefault("capture_output", True)
    kwargs.setdefault("text", True)
    return subprocess.run(list(args), **kwargs)


def check_tool(name: str) -> bool:
    return shutil.which(name) is not None


def setup_work_dir():
    if WORK_DIR.exists():
        shutil.rmtree(WORK_DIR)
    WORK_DIR.mkdir(parents=True, exist_ok=True)


def cleanup_work_dir():
    if WORK_DIR.exists():
        shutil.rmtree(WORK_DIR)


# ═══════════════════════════════════════════════════════════════════
#  ZIP 创建 (pyzipper — 支持 AES 和 ZipCrypto 加密)
# ═══════════════════════════════════════════════════════════════════

def create_encrypted_zip(output: Path, files: dict, password: str,
                         encryption=pyzipper.WZ_AES):
    """
    使用 pyzipper 创建加密 ZIP。

    files: {arcname: str | bytes} (内容是文件数据)
    """
    output.parent.mkdir(parents=True, exist_ok=True)
    with pyzipper.AESZipFile(output, 'w',
                             compression=pyzipper.ZIP_DEFLATED,
                             encryption=encryption) as zf:
        zf.setpassword(password.encode('utf-8'))
        for arcname, content in files.items():
            data = content.encode('utf-8') if isinstance(content, str) else content
            zf.writestr(arcname, data)


def create_plain_zip(output: Path, files: dict):
    """使用 pyzipper 创建无密码 ZIP."""
    output.parent.mkdir(parents=True, exist_ok=True)
    with pyzipper.AESZipFile(output, 'w',
                             compression=pyzipper.ZIP_DEFLATED,
                             encryption=None) as zf:
        for arcname, content in files.items():
            data = content.encode('utf-8') if isinstance(content, str) else content
            zf.writestr(arcname, data)


def add_files_to_zip(zf: pyzipper.AESZipFile, files: dict):
    """向已打开的 zip 添加文件."""
    for arcname, content in files.items():
        data = content.encode('utf-8') if isinstance(content, str) else content
        zf.writestr(arcname, data)


# ═══════════════════════════════════════════════════════════════════
#  7z 创建 (py7zr — 支持 AES-256 加密)
# ═══════════════════════════════════════════════════════════════════

def create_encrypted_7z(output: Path, files: dict, password: str):
    """
    使用 py7zr 创建加密 7z。

    files: {arcname: str | bytes}
    """
    output.parent.mkdir(parents=True, exist_ok=True)
    work = WORK_DIR / output.stem
    work.mkdir(parents=True, exist_ok=True)
    try:
        for arcname, content in files.items():
            target = work / arcname
            target.parent.mkdir(parents=True, exist_ok=True)
            data = content.encode('utf-8') if isinstance(content, str) else content
            target.write_bytes(data)

        with py7zr.SevenZipFile(output, 'w', password=password) as zf:
            for arcname in files:
                zf.write(str(work / arcname), arcname=arcname)
    finally:
        shutil.rmtree(work)


# ═══════════════════════════════════════════════════════════════════
#  低级 ZIP (用于编码测试: 非 UTF-8 文件名)
# ═══════════════════════════════════════════════════════════════════

def create_zip_with_encoding(output: Path,
                             files: dict[str, bytes],  # {filename_bytes_in_encoding: content_bytes}
                             use_utf8_flag: bool = False):
    """
    创建使用特定编码文件名的 ZIP（无密码，用于编码检测测试）。
    """
    import zlib
    import struct as st

    output.parent.mkdir(parents=True, exist_ok=True)
    entries = []

    for fname_bytes, content in files.items():
        info = _zipfile.ZipInfo()
        info.filename = fname_bytes.decode("latin-1")
        info.date_time = _time.localtime(_time.time())[:6]
        info.compress_type = _zipfile.ZIP_DEFLATED
        if use_utf8_flag:
            info.flag_bits |= 0x0800
        compressed = zlib.compress(content)
        info.CRC = zlib.crc32(content) & 0xFFFFFFFF
        info.file_size = len(content)
        info.compress_size = len(compressed)
        entries.append((info, compressed, fname_bytes))

    with open(output, "wb") as f:
        for info, compressed, raw_fname in entries:
            info.header_offset = f.tell()
            f.write(b"PK\003\004")
            fn = raw_fname if use_utf8_flag else info.filename.encode("latin-1")
            f.write(st.pack("<HHHHHIIIHH",
                20, info.flag_bits, info.compress_type,
                0, 0, info.CRC, info.compress_size, info.file_size,
                len(fn), 0))
            f.write(fn)
            f.write(compressed)

        cd_offset = f.tell()
        for info, compressed, raw_fname in entries:
            f.write(b"PK\001\002")
            fn = raw_fname if use_utf8_flag else info.filename.encode("latin-1")
            f.write(st.pack("<HHHHHHIIIHHHHHII",
                20, 20, info.flag_bits, info.compress_type,
                0, 0, info.CRC, info.compress_size, info.file_size,
                len(fn), 0, 0, 0, 0, 0, info.header_offset))
            f.write(fn)

        cd_size = f.tell() - cd_offset
        f.write(b"PK\005\006")
        f.write(st.pack("<HHHHIIH",
            0, 0, len(entries), len(entries), cd_size, cd_offset, 0))


# ═══════════════════════════════════════════════════════════════════
#  嵌套压缩包辅助
# ═══════════════════════════════════════════════════════════════════

def zip_add_archive(output: Path, inner_path: Path, inner_name: str,
                    password: str = None):
    """
    创建一个 zip，其中包含另一个压缩包文件。

    读取 inner_path 的二进制内容，以 inner_name 写入 ZIP。
    """
    output.parent.mkdir(parents=True, exist_ok=True)
    data = inner_path.read_bytes()
    enc = pyzipper.WZ_AES if password else None
    with pyzipper.AESZipFile(output, 'w',
                             compression=pyzipper.ZIP_DEFLATED,
                             encryption=enc) as zf:
        if password:
            zf.setpassword(password.encode('utf-8'))
        zf.writestr(inner_name, data)


# ═══════════════════════════════════════════════════════════════════
#  1. 嵌套压缩包
# ═══════════════════════════════════════════════════════════════════

def gen_nested_zip_in_zip():
    name = "nested_zip_in_zip"
    print(f"  [{name}] 生成中...")
    # 内层: hello.txt
    inner = WORK_DIR / "inner.zip"
    create_plain_zip(inner, {"hello.txt": "Hello from inner zip!\n"})
    # 外层: 包含 inner.zip
    zip_add_archive(FIXTURES_DIR / f"{name}.zip", inner, "inner.zip")
    print(f"    -> {FIXTURES_DIR / f'{name}.zip'}")


def gen_nested_7z_in_zip():
    name = "nested_7z_in_zip"
    print(f"  [{name}] 生成中...")
    inner = WORK_DIR / "inner.7z"
    create_encrypted_7z(inner, {"data.txt": "Data in 7z archive\n"}, password="")
    zip_add_archive(FIXTURES_DIR / f"{name}.zip", inner, "inner.7z")
    print(f"    -> {FIXTURES_DIR / f'{name}.zip'}")


def gen_nested_multi_level():
    name = "nested_multi_level"
    print(f"  [{name}] 生成中...")
    l3 = WORK_DIR / "L3.zip"
    create_plain_zip(l3, {"deep.txt": "This is really deep!\n"})
    l2 = WORK_DIR / "L2.zip"
    zip_add_archive(l2, l3, "L3.zip")
    zip_add_archive(FIXTURES_DIR / f"{name}.zip", l2, "L2.zip")
    print(f"    -> {FIXTURES_DIR / f'{name}.zip'}")


def gen_nested_mixed_formats():
    name = "nested_mixed_formats"
    print(f"  [{name}] 生成中...")
    # a.zip
    a_zip = WORK_DIR / "a.zip"
    create_plain_zip(a_zip, {"a_inside.txt": "Inside a.zip\n"})
    # b.7z
    b_7z = WORK_DIR / "b.7z"
    create_encrypted_7z(b_7z, {"b_inside.txt": "Inside b.7z\n"}, password="")
    # c.tar.gz
    c_tgz = WORK_DIR / "c.tar.gz"
    write_text(WORK_DIR / "c_inside.txt", "Inside c.tar.gz\n")
    with tarfile.open(c_tgz, "w:gz") as tf:
        tf.add(WORK_DIR / "c_inside.txt", arcname="c_inside.txt")

    # 外层: 包含全部
    outer = FIXTURES_DIR / f"{name}.zip"
    files_data = {}
    for p, n in [(a_zip, "a.zip"), (b_7z, "b.7z"), (c_tgz, "c.tar.gz")]:
        files_data[n] = p.read_bytes()
    create_plain_zip(outer, files_data)
    print(f"    -> {outer}")


# ═══════════════════════════════════════════════════════════════════
#  1b. 嵌套归档路径冲突测试夹具 (tar.gz 与 archive_stem 等价名称)
# ═══════════════════════════════════════════════════════════════════

def _create_tar_gz_with_leaf(name: str, leaf_filename: str, leaf_content: str):
    """
    Create a .tar.gz where the inner tar name equals archive_stem
    (e.g. name='real_tar' → real_tar.tar.gz → real_tar.tar → leaf).
    This triggers CommitSingleFileAsInnerName path collision.
    """
    tar_path = WORK_DIR / f"{name}.tar"
    leaf_path = WORK_DIR / leaf_filename
    leaf_path.write_text(leaf_content)
    with tarfile.open(tar_path, "w") as tf:
        tf.add(leaf_path, arcname=leaf_filename)
    tgz_path = FIXTURES_DIR / f"{name}.tar.gz"
    with gzip.open(tgz_path, "wb") as gf:
        gf.write(tar_path.read_bytes())
    print(f"    -> {tgz_path}  ({tgz_path.stat().st_size} B)")


def gen_nested_path_collision_fixtures():
    print("\n[1b] 嵌套归档路径冲突夹具")

    # real_tar.tar.gz — archive_stem='real_tar', inner tar='real_tar.tar'
    # → Equivalent name_similarity → CommitSingleFileAsInnerName → path collision
    _create_tar_gz_with_leaf("real_tar", "leaf_rt.txt", "leaf content from real_tar\n")

    # matching.tar.gz — same equivalence trigger, different name
    _create_tar_gz_with_leaf("matching", "leaf_m.txt", "leaf from matching\n")

    # zip_containing_real_tar_gz.zip — outer zip with single-entry tar.gz
    inner_tgz = FIXTURES_DIR / "real_tar.tar.gz"
    zip_path = FIXTURES_DIR / "zip_containing_real_tar_gz.zip"
    with pyzipper.AESZipFile(zip_path, 'w', compression=pyzipper.ZIP_DEFLATED, encryption=None) as zf:
        zf.writestr("real_tar.tar.gz", inner_tgz.read_bytes())
    print(f"    -> {zip_path}  ({zip_path.stat().st_size} B)")

    # zip_inner_zip.zip — outer zip contains a single inner archive named
    # like the outer archive stem, forcing CommitSingleFileAsInnerName for
    # an archive file before nested extraction continues.
    inner_zip_path = WORK_DIR / "zip_inner_zip_payload.zip"
    with pyzipper.AESZipFile(inner_zip_path, 'w', compression=pyzipper.ZIP_DEFLATED, encryption=None) as zf:
        zf.writestr("zip_inner_leaf.txt", "leaf from inner zip\n")
    zip_inner_zip_path = FIXTURES_DIR / "zip_inner_zip.zip"
    with pyzipper.AESZipFile(zip_inner_zip_path, 'w', compression=pyzipper.ZIP_DEFLATED, encryption=None) as zf:
        zf.writestr("zip_inner_zip.zip", inner_zip_path.read_bytes())
    print(f"    -> {zip_inner_zip_path}  ({zip_inner_zip_path.stat().st_size} B)")


# ═══════════════════════════════════════════════════════════════════
#  2. Unicode 密码加密压缩包
# ═══════════════════════════════════════════════════════════════════

def gen_unicode_passwords():
    tests = [
        ("pass_cn.zip",     "中文密码123",       {"文档.txt": "这是中文密码保护的 zip 文件内容。\n"},          "zip"),
        ("pass_jp.7z",      "日本語パスワード",   {"ファイル.txt": "日本語のパスワードで保護された7zです。\n"}, "7z"),
        ("pass_kr.zip",     "한국어비밀번호",     {"문서.txt": "한국어 비밀번호로 보호된 zip 파일.\n"},        "zip"),
        ("pass_emoji.zip",  "🔒Secret!密码",      {"readme.txt": "Emoji + mixed language password!\n"},     "zip"),
        ("pass_rtl.zip",    "עברית-123",         {"readme.txt": "Right-to-left password archive.\n"},      "zip"),
    ]
    for filename, password, files, fmt in tests:
        print(f"  [{filename}] 密码: {password}")
        if fmt == "zip":
            create_encrypted_zip(FIXTURES_DIR / filename, files, password)
        else:
            create_encrypted_7z(FIXTURES_DIR / filename, files, password)


# ═══════════════════════════════════════════════════════════════════
#  3. 视频文件内嵌压缩包
# ═══════════════════════════════════════════════════════════════════

def _make_tiny_video(output: Path):
    """使用 ffmpeg 创建极小的测试视频."""
    result = run_cmd(
        "ffmpeg", "-y",
        "-f", "lavfi", "-i", "color=c=blue:s=32x32:d=0.1",
        "-c:v", "libx264", "-preset", "ultrafast",
        "-t", "0.3",
        str(output),
    )
    if result.returncode != 0:
        print(f"    警告: ffmpeg 失败: {result.stderr}")
        output.write_bytes(b"\x00" * 1024)


def gen_video_embedded(tools: dict):
    if not tools["ffmpeg"]:
        print("  [跳过] ffmpeg 不可用")
        return

    # --- video_zip.mp4 ---
    print("  [video_zip.mp4] 生成中...")
    video = WORK_DIR / "tiny.mp4"
    _make_tiny_video(video)
    hidden = WORK_DIR / "hidden.zip"
    create_plain_zip(hidden, {"hidden.txt": "Found inside video!\n"})
    out = FIXTURES_DIR / "video_zip.mp4"
    with open(out, "wb") as f:
        f.write(video.read_bytes())
        f.write(hidden.read_bytes())
    print(f"    -> {out}")

    # --- video_7z_pass.mp4 ---
    print("  [video_7z_pass.mp4] 生成中...")
    video2 = WORK_DIR / "tiny2.mp4"
    _make_tiny_video(video2)
    secret = WORK_DIR / "secret.7z"
    create_encrypted_7z(secret, {"secret.txt": "Secret in video!\n"}, password="video-pass")
    out2 = FIXTURES_DIR / "video_7z_pass.mp4"
    with open(out2, "wb") as f:
        f.write(video2.read_bytes())
        f.write(secret.read_bytes())
    print(f"    -> {out2}")


# ═══════════════════════════════════════════════════════════════════
#  4. 各种编码方式的压缩包
# ═══════════════════════════════════════════════════════════════════

def gen_encoding_variants():
    print("  [enc_gbk.zip] GBK 编码文件名")
    create_zip_with_encoding(FIXTURES_DIR / "enc_gbk.zip", {
        "中文文件名测试.txt".encode("gbk"): "GBK ZIP content\n".encode("utf-8"),
        "压缩包说明文档.doc".encode("gbk"): "GBK doc\n".encode("utf-8"),
    })

    print("  [enc_sjis.zip] Shift_JIS 编码文件名")
    create_zip_with_encoding(FIXTURES_DIR / "enc_sjis.zip", {
        "日本語ファイル名テスト.txt".encode("shift_jis"): "Shift_JIS ZIP\n".encode("utf-8"),
        "資料/会議メモ.docx".encode("shift_jis"): "Shift_JIS doc\n".encode("utf-8"),
    })

    print("  [enc_euckr.zip] EUC-KR 编码文件名")
    create_zip_with_encoding(FIXTURES_DIR / "enc_euckr.zip", {
        "한글파일이름.txt".encode("euc_kr"): "EUC-KR ZIP\n".encode("utf-8"),
        "보고서_2024.hwp".encode("euc_kr"): "EUC-KR doc\n".encode("utf-8"),
    })

    print("  [enc_big5.zip] Big5 编码文件名")
    create_zip_with_encoding(FIXTURES_DIR / "enc_big5.zip", {
        "繁體中文檔案名稱.txt".encode("big5"): "Big5 ZIP\n".encode("utf-8"),
        "會議記錄.doc".encode("big5"): "Big5 doc\n".encode("utf-8"),
    })

    print("  [enc_utf8.zip] UTF-8 编码文件名 (对照组)")
    create_plain_zip(FIXTURES_DIR / "enc_utf8.zip", {
        "中文文件名测试.txt": "UTF-8 ZIP\n",
        "日本語テスト.txt": "UTF-8 ZIP\n",
        "한국어테스트.txt": "UTF-8 ZIP\n",
        "English_File.txt": "Normal English\n",
    })


# ═══════════════════════════════════════════════════════════════════
#  5. 组合场景
# ═══════════════════════════════════════════════════════════════════

def gen_combo_nested_pass():
    name = "combo_nested_pass"
    print(f"  [{name}] 生成中...")
    inner = WORK_DIR / "inner_pass.zip"
    create_encrypted_zip(inner, {"机密文件.txt": "这是内层密码保护的内容。\n"},
                         password="内层密码")
    zip_add_archive(FIXTURES_DIR / f"{name}.zip", inner, "inner_pass.zip")
    print(f"    -> {FIXTURES_DIR / f'{name}.zip'} (外层无密码, 内层密码: 内层密码)")


def gen_combo_video_nested(tools: dict):
    name = "combo_video_nested"
    if not tools["ffmpeg"]:
        print(f"  [{name}] 跳过: ffmpeg 不可用")
        return
    print(f"  [{name}] 生成中...")
    deep = WORK_DIR / "deep_pass.zip"
    create_encrypted_zip(deep, {"found.txt": "You found me!\n"}, password="deep-🔑")
    outer_zip = WORK_DIR / "outer.zip"
    zip_add_archive(outer_zip, deep, "deep_pass.zip")
    video = WORK_DIR / "video.mp4"
    _make_tiny_video(video)
    out = FIXTURES_DIR / f"{name}.mp4"
    with open(out, "wb") as f:
        f.write(video.read_bytes())
        f.write(outer_zip.read_bytes())
    print(f"    -> {out}")


def gen_combo_multipass():
    name = "combo_multipass"
    print(f"  [{name}] 生成中...")
    a_zip = WORK_DIR / "a.zip"
    create_encrypted_zip(a_zip, {"a_content.txt": "This is A.\n"}, password="密码Alpha")
    b_zip = WORK_DIR / "b.zip"
    create_encrypted_zip(b_zip, {"b_content.txt": "This is B.\n"}, password="パスワードBeta")
    outer = FIXTURES_DIR / f"{name}.zip"
    files_data = {}
    for p, n in [(a_zip, "a.zip"), (b_zip, "b.zip")]:
        files_data[n] = p.read_bytes()
    create_plain_zip(outer, files_data)
    print(f"    -> {outer} (a.zip 密码: 密码Alpha, b.zip 密码: パスワードBeta)")


# ═══════════════════════════════════════════════════════════════════
#  附加: RAR 夹具 (需 CLI rar)
# ═══════════════════════════════════════════════════════════════════

def gen_rar_fixtures(tools: dict):
    if not tools["rar"]:
        print("  [跳过] rar 工具不可用")
        return
    print("  [pass_rar.rar] 密码: rar密码!")
    write_text(WORK_DIR / "rar_content.txt", "RAR archive with password.\n")
    r = run_cmd("rar", "a", "-prar密码!",
                str(FIXTURES_DIR / "pass_rar.rar"),
                str(WORK_DIR / "rar_content.txt"))
    if r.returncode == 0:
        print(f"    -> {FIXTURES_DIR / 'pass_rar.rar'}")
    else:
        print(f"    警告: {r.stderr}")

    print("  [split_rar.part1.rar] 分卷 RAR")
    write_text(WORK_DIR / "large_data.bin", "X" * 100, encoding="ascii")
    r = run_cmd("rar", "a", "-v10b",
                str(FIXTURES_DIR / "split_rar.rar"),
                str(WORK_DIR / "large_data.bin"))
    if r.returncode == 0:
        print(f"    -> {FIXTURES_DIR}/split_rar.part*.rar")


# ═══════════════════════════════════════════════════════════════════
#  工具函数
# ═══════════════════════════════════════════════════════════════════

def write_text(path: Path, content: str, encoding: str = "utf-8"):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding=encoding)


# ═══════════════════════════════════════════════════════════════════
#  主流程
# ═══════════════════════════════════════════════════════════════════

PASSWORD_TABLE = """
密码速查表:
  pass_cn.zip        中文密码123
  pass_jp.7z         日本語パスワード
  pass_kr.zip        한국어비밀번호
  pass_emoji.zip     🔒Secret!密码
  pass_rtl.zip       עברית-123
  combo_nested_pass  外层无密码, 内层 内层密码
  combo_multipass    a.zip→密码Alpha, b.zip→パスワードBeta
  combo_video_nested 内嵌 → deep-🔑
  video_7z_pass      内嵌 7z → video-pass
"""


def main():
    parser = argparse.ArgumentParser(description="SmartZip 测试夹具生成器")
    parser.add_argument("--force", action="store_true", help="强制重新生成")
    args = parser.parse_args()

    tools = {
        "ffmpeg": check_tool("ffmpeg"),
        "rar": check_tool("rar"),
    }

    print("=" * 60)
    print("SmartZip 测试夹具生成器 (Python 库版)")
    print("=" * 60)
    print(f"\n[pip 依赖] pyzipper, py7zr")
    print(f"[可选工具] ffmpeg: {'✓' if tools['ffmpeg'] else '✗'}, rar: {'✓' if tools['rar'] else '✗'}")

    # 检查已有夹具
    existing = [f for f in FIXTURES_DIR.glob("*")
                if f.suffix in (".zip", ".7z", ".mp4", ".rar")
                and f.name not in ("generate.py", "README.md")]

    if existing and not args.force:
        print(f"\n发现 {len(existing)} 个已有夹具:")
        for f in sorted(existing):
            print(f"  - {f.name}")
        print("\n使用 --force 强制重新生成")
        return

    if args.force and existing:
        print(f"\n强制模式: 删除 {len(existing)} 个已有夹具...")
        for f in existing:
            f.unlink()

    setup_work_dir()

    try:
        print("\n[1/5] 嵌套压缩包")
        gen_nested_zip_in_zip()
        gen_nested_7z_in_zip()
        gen_nested_multi_level()
        gen_nested_mixed_formats()
        gen_nested_path_collision_fixtures()

        print("\n[2/5] Unicode 密码加密压缩包")
        gen_unicode_passwords()

        print("\n[3/5] 视频文件内嵌压缩包")
        gen_video_embedded(tools)

        print("\n[4/5] 各种编码方式的压缩包")
        gen_encoding_variants()

        print("\n[5/5] 组合场景")
        gen_combo_nested_pass()
        gen_combo_video_nested(tools)
        gen_combo_multipass()

        print("\n[额外] RAR 测试夹具")
        gen_rar_fixtures(tools)

        generated = sorted(
            [f for f in FIXTURES_DIR.glob("*")
             if f.suffix in (".zip", ".7z", ".mp4", ".rar")
             and f.name not in ("generate.py", "README.md")]
        )
        print("\n" + "=" * 60)
        print(f"生成完成! 共 {len(generated)} 个夹具:")
        print("=" * 60)
        for f in generated:
            size_kb = f.stat().st_size / 1024
            print(f"  {f.name:40s} {size_kb:8.1f} KB")
        print(PASSWORD_TABLE)

    finally:
        cleanup_work_dir()


if __name__ == "__main__":
    main()
