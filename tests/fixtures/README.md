# SmartZip 测试夹具

本目录包含用于 SmartZip 集成测试的压缩包夹具生成脚本和说明。

## 测试夹具类型

| 类别 | 夹具 | 说明 |
|------|------|------|
| **嵌套压缩包** | `nested_zip_in_zip.zip` | zip 内包含另一个 zip |
| | `nested_7z_in_zip.zip` | zip 内包含 7z |
| | `nested_multi_level.zip` | 三层嵌套 zip → zip → zip |
| | `nested_mixed_formats.zip` | zip 内包含 zip + 7z + tar.gz |
| **Unicode 密码** | `pass_cn.zip` | 密码: `中文密码123` |
| | `pass_jp.7z` | 密码: `日本語パスワード` |
| | `pass_kr.zip` | 密码: `한국어비밀번호` |
| | `pass_emoji.zip` | 密码: `🔒Secret!密码` |
| | `pass_rtl.zip` | 密码: `עברית-123` (希伯来语) |
| **视频内嵌压缩包** | `video_zip.mp4` | mp4 视频末尾拼接 zip |
| | `video_7z_pass.mp4` | mp4 视频末尾拼接加密 7z |
| **编码变体** | `enc_gbk.zip` | GBK 编码文件名 |
| | `enc_sjis.zip` | Shift_JIS 编码文件名 |
| | `enc_euckr.zip` | EUC-KR 编码文件名 |
| | `enc_big5.zip` | Big5 编码文件名 |
| | `enc_utf8.zip` | UTF-8 编码文件名（对照组） |
| **组合场景** | `combo_nested_pass.zip` | 嵌套 + unicode 密码 |
| | `combo_video_nested.mp4` | 视频 + 内嵌加密嵌套压缩包 |
| | `combo_multipass.zip` | 外层无密码，内层各有不同密码 |

## 所需软件工具

### Linux (Ubuntu/Debian)

```bash
# 核心工具 (必须)
sudo apt install -y p7zip-full     # 7z 命令 (创建/解压 zip, 7z 等)
sudo apt install -y python3        # 夹具生成脚本

# RAR 支持 (可选, 用于 rar 相关测试)
# 方法1: 安装非自由 rar 包
sudo apt install -y rar unrar
# 方法2: 或从 https://www.rarlab.com/download.htm 下载 Linux 版 rar

# 视频测试 (可选, 用于视频内嵌测试)
sudo apt install -y ffmpeg

# 标准工具 (一般已安装)
# tar, gzip, bzip2, xz, zip, unzip, cat
```

### Linux (Arch)

```bash
sudo pacman -S p7zip python ffmpeg
# RAR: yay -S rar unrar (AUR)
```

### Linux (Fedora)

```bash
sudo dnf install -y p7zip p7zip-plugins python3 ffmpeg
# RAR 需要从 RPM Fusion 或 rarlab 下载
```

### macOS

```bash
brew install p7zip python3 ffmpeg
# RAR: brew install --cask rar
```

### Windows

```powershell
# 安装 7-Zip: https://www.7-zip.org/
# 安装 Python: https://www.python.org/downloads/
# 安装 ffmpeg: https://ffmpeg.org/download.html
# 安装 WinRAR: https://www.rarlab.com/download.htm
```

## 生成夹具

```bash
cd tests/fixtures
python3 generate.py
```

生成的 `.zip` / `.7z` / `.mp4` 文件均在 `tests/fixtures/` 目录下。

生成的所有夹具已加入 `.gitignore`，不纳入版本控制。

## 运行集成测试

```bash
# 先生成夹具，再运行测试
cd tests/fixtures && python3 generate.py && cd ../..
cargo test --test smartzip_integration -- --test-threads=1

# 或运行全部测试
cargo test -- --test-threads=1
```

## 夹具清单与密码表

| 夹具文件 | 密码 | 内部内容 |
|----------|------|----------|
| `nested_zip_in_zip.zip` | (无) | `inner.zip` 包含 `hello.txt` |
| `nested_7z_in_zip.zip` | (无) | `inner.7z` 包含 `data.txt` |
| `nested_multi_level.zip` | (无) | L1.zip → L2.zip → `deep.txt` |
| `nested_mixed_formats.zip` | (无) | `a.zip` + `b.7z` + `c.tar.gz` |
| `pass_cn.zip` | `中文密码123` | `文档.txt` |
| `pass_jp.7z` | `日本語パスワード` | `ファイル.txt` |
| `pass_kr.zip` | `한국어비밀번호` | `문서.txt` |
| `pass_emoji.zip` | `🔒Secret!密码` | `readme.txt` |
| `pass_rtl.zip` | `עברית-123` | `readme.txt` |
| `video_zip.mp4` | (无, 内嵌) | 视频 + zip(含 `hidden.txt`) |
| `video_7z_pass.mp4` | `video-pass` (内嵌) | 视频 + 加密7z(含 `secret.txt`) |
| `enc_gbk.zip` | (无) | GBK 编码中文文件名 |
| `enc_sjis.zip` | (无) | Shift_JIS 编码日文文件名 |
| `enc_euckr.zip` | (无) | EUC-KR 编码韩文文件名 |
| `enc_big5.zip` | (无) | Big5 编码繁体中文文件名 |
| `combo_nested_pass.zip` | 外层无, 内层 `内层密码` | `outer.zip` → `inner_pass.zip` |
| `combo_video_nested.mp4` | 内嵌 `deep-🔑` | 视频 + 加密zip → 加密zip |
| `combo_multipass.zip` | 外层无 | a.zip(密码A), b.zip(密码B) |
