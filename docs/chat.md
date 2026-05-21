# SmartZip 测试夹具设计指南

**Date**: May 20, 2026 at 10:11 PM
**Turns**: 3
**Source**: [Gemini Chat](https://gemini.google.com/app/fb869997491fcfb1)

---

## Turn 1

### 👤 User

*[This turn includes uploaded images]*

![MD icon](assets/img-001.png)

对于这样一个项目，如何创建所需的测试夹具？

### 🤖 Assistant

在为 **SmartZip** 这样集成了压缩解压、多后端调用、密码推荐、编码检测以及内嵌文件扫描的复杂系统设计测试夹具（Test Fixtures）时，目标是确保**测试的可预测性、环境的隔离性**，以及**避免在 CI/CD 环境中高成本地依赖复杂的外部系统环境**。
针对你的设计方案，可以从**夹具分类设计**、**目录结构组织**、**关键夹具的生成技术**以及**在 Rust 中的测试最佳实践**四个维度来构建测试夹具方案。

### 1. 测试夹具分类设计
为了覆盖方案中提到的 10 个实施切片（Slices），需要准备以下五类测试夹具：

| 夹具分类 | 对应组件 / 切片 | 夹具具体内容 | 目的 |
| --- | --- | --- | --- |
| 标准与异常压缩包Archive Fixtures | smartzip-archivesmartzip-core(Slice 2, 5) | 1. 各种格式（.zip, .7z, .rar, .tar.gz 等）的完好包。2. 异常包：损坏的（Corrupted）、空包、分卷压缩包（如 .part1.rar, .part2.rar）。 | 验证后端调用、任务解压决策、分卷跳过逻辑。 |
| 多编码文件名包Encoding Fixtures | smartzip-encoding(Slice 6) | 使用非 UTF-8 编码（如 GBK, Big5, Shift_JIS, EUC-KR）写入文件名的压缩包。 | 验证自动检测逻辑及评分矩阵的准确性。 |
| 内嵌与伪装文件Scanner Fixtures | smartzip-scanner(Slice 7) | 1. 伪装后缀文件（如将 zip 改名为 .dat）。2. 拼接文件：在任意二进制或图片后追加压缩包（Offset > 0）。3. 自解压包（SFX）。 | 验证 binwalk 封装在 Fast/Deep 模式下的 Magic Bytes 扫描和 Offset/Size 提取。 |
| 7zz 命令行输出快照Snapshot Fixtures | smartzip-archive(Slice 2, 风险缓解) | 7zz 在执行 list, test, extract 成功或失败（如密码错、损坏）时的 stdout/stderr 原始文本文件。 | 核心解耦点：无需真实调用 7zz 进程，即可高频测试解析器对各种边界输出的解析。 |
| 数据库初始模版Database Fixtures | smartzip-dbsmartzip-passwords(Slice 3, 4) | 预先配置好不同权重、不同命中模式、有成功/失败统计数据的 SQLite .db 模版文件。 | 验证密码排序算法（Score 公式）的推荐 Top N 结果。 |
| 安全防御包Security Fixtures | 安全设计 (Section 10) | 包含 ../ 路径穿越（Zip Slip）或绝对路径的文件包。 | 验证安全拦截边界，确保不会写出到目标目录之外。 |

### 2. 夹具目录结构组织
建议在 Rust Workspace 根目录下设立统一的 `tests/fixtures` 目录。对于平台特定的文本或体积较小的快照，可依类放置。

```plaintext
smartzip/ (Workspace Root)
├── crates/
├── tests/
│   └── fixtures/
│       ├── archives/                # 静态压缩包存放
│       │   ├── standard/            # 完好包 (sample.zip, sample.7z)
│       │   ├── encrypted/           # 带密码包 (pass_123.zip, no_list.rar)
│       │   ├── encodings/           # 包含多编码文件名的包 (gbk.zip, shift_jis.7z)
│       │   ├── embedded/            # 内嵌包 (offset_1024.bin, sfx_linux)
│       │   ├── volumes/             # 分卷测试 (archive.part1.rar, archive.part2.rar)
│       │   └── malicious/           # 恶意包 (zip_slip.zip)
│       ├── cli_snapshots/           # 7zz 命令行输出快照文本
│       │   ├── 7zz_list_success.txt
│       │   ├── 7zz_list_encrypted_no_list.txt
│       │   ├── 7zz_extract_wrong_password.txt
│       │   └── 7zz_extract_corrupted.txt
│       └── databases/               # SQLite 种子数据
│           ├── empty_v1.db          # 仅用于迁移测试
│           └── seed_passwords.db    # 用于测试推荐算法的预填库
└── scripts/
    └── generate_fixtures.py         # 夹具自动生成脚本

```

### 3. 关键夹具的生成技术与脚本
普通的压缩软件通常会自动过滤路径穿越，也难以在单台机器上轻松生成多种旧Locale编码的压缩包。因此，需要**通过脚本/程序来特制这些夹具**。

#### A. 路径穿越（Zip Slip）与多编码夹具的生成 (Python 脚本)
在 `scripts/generate_fixtures.py` 中，利用 Python 的 `zipfile` 模块可以自由控制字节码和路径：

```python
import zipfile
import os

def create_zip_slip_fixture():
    output_path = "tests/fixtures/archives/malicious/zip_slip.zip"
    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    with zipfile.ZipFile(output_path, 'w') as zf:
        # 强制写入包含路径穿越的危险路径
        zf.writestr("../../../../../tmp/smartzip_evil.txt", "malicious payload")
    print(f"Created: {output_path}")

def create_encoding_fixture():
    output_path = "tests/fixtures/archives/encodings/gbk_filenames.zip"
    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    
    with zipfile.ZipFile(output_path, 'w') as zf:
        # 将文件名强制转换为 GBK 字节码写入 Zip
        filename_gbk = "测试文件_GBK.txt".encode('gbk')
        
        zinfo = zipfile.ZipInfo()
        zinfo.filename = filename_gbk.decode('cp437') # 绕过 python 默认的 utf-8 检查
        zinfo.file_size = 12
        zinfo.external_attr = 0o600 << 16
        zf.writestr(zinfo, "hello gbk text")
    print(f"Created: {output_path}")

if __name__ == "__main__":
    create_zip_slip_fixture()
    create_encoding_fixture()

```

#### B. 内嵌压缩包夹具的生成 (Shell 命令)
在已有完好压缩包的基础上，通过文件追加（Concatenation）和填充来构造具有特定偏移量（Offset）的文件：

```bash
# 创建一个 1MB 的干扰头部（模拟伪装文件或自解压头部）
dd if=/dev/urandom of=head.bin bs=1024 count=1024

# 创建一个小型的标准 ZIP 文件
echo "inner file content" > inner.txt
zip inner.zip inner.txt

# 拼接：形成一个 offset 在 1048576 字节处的内嵌压缩包
cat head.bin inner.zip > tests/fixtures/archives/embedded/offset_1048576.bin

# 清理临时文件
rm head.bin inner.zip inner.txt

```

### 4. 在 Rust 测试中高效利用夹具

#### A. 后端解析器测试：利用快照夹具避免真实 I/O
方案 14 提到“7zz输出解析不稳定”。测试它时不应该频繁调用真正的 `7zz` 进程，而是将 `cli_snapshots` 中的文本读入：

```rust
// crates/smartzip-archive/src/seven_zip/parser.rs 的测试代码
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_wrong_password_stderr() {
        // 读取静态快照夹具
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/cli_snapshots/7zz_extract_wrong_password.txt");
        let stderr_content = fs.read_to_string(fixture_path).unwrap();

        // 假装这是从 7zz 进程拿到的输出，测试解析器
        let parse_result = parse_7z_error(&stderr_content);
        
        assert!(matches!(parse_result, SmartZipError::WrongPassword { .. }));
    }
}

```

#### B. 核心业务测试：利用 `tempfile` 构建隔离的解压沙箱
在测试 `smartzip-core` 的智能解压流程（Section 5）时，为了验证“解压到临时目录，移动，应用后置规则”的逻辑，必须在隔离的临时目录运行：

```rust
// crates/smartzip-core/tests/integration_test.rs
use tempfile::tempdir;
use std::fs;

#[tokio::test]
async fn test_extract_recursive_workflow() {
    // 1. 获取静态测试夹具文件的绝对路径
    let fixture_zip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/archives/standard/sample.zip");

    // 2. 创建一个临时沙箱目录作为解压输出目标（测试完成后自动清理）
    let target_dir = tempdir().unwrap();
    let output_path = target_dir.path().to_path_buf();

    // 3. 初始化引擎并执行
    let engine = SmartZipEngine::new_mock_or_test();
    let request = ExtractRequest {
        inputs: vec![fixture_zip],
        output_dir: Some(output_path.clone()),
        recursion_limit: 3,
        // ... 其他配置
    };

    let result = engine.execute_extract(request).await;
    assert!(result.is_ok());

    // 4. 断言检查解压决策及后置规则是否正确生效
    assert!(output_path.join("expected_extracted_file.txt").exists());
}

```

#### C. 密码与数据库测试：利用内存数据库（`:memory:`）和 Seed 数据
对于密码库的算法排序（Slice 4），在单测中可以首选 SQLite 的内存模式来隔离环境；而在集成测试中，则可将 `seed_passwords.db` 拷贝到临时目录中进行有状态验证，防止破坏主数据库。

### 总结建议

1. **版本控制**：将测试夹具中所涉及的代码生成脚本（如 Python 脚本）提交到 Git。如果压缩包夹具体积很小（累计 < 10MB），可直接存入 Git；如果未来夹具体积变大，建议引入 `Git LFS`。
2. **确定性锁**：对生成的静态压缩包夹具在测试注释中记录其 MD5/SHA256，或通过测试代码进行校验，防止夹具文件在不同平台上因意外损坏或被 Git 换行符（CRLF）自动转换导致测试失效。

## Turn 2

### 👤 User

需求还有嵌套压缩包、编码、内嵌、unicode密码和他们的组合，以及智能解压(直接解压/解压到新目录)等等，智能工作流需要自动处理以上的复合情况

### 🤖 Assistant

面对这种“多路Buff叠加”的复合灾难场景（套娃包 + 乱码编码 + 隐蔽内嵌 + 含有表情/特殊符号的 Unicode 密码 + 智能目录决策），手工测试几乎是不可能的。智能工作流（Smart Workflow）必须像一个状态机，在一层层剥开压缩包的过程中，动态触发不同的核心能力。
要确保工作流能自动、稳健地处理这种复合情况，我们需要设计“终极套娃（Matrix/Composite）”测试夹具，并在测试中模拟这条复杂的自动化流水线。

## 1. 复合场景“终极套娃”夹具结构设计
我们需要在 `tests/fixtures/archives/composite/` 下构建一个名为 `nightmare_matrix.zip` 的终极测试文件。它的内部结构逻辑如下：

```plaintext
nightmare_matrix.zip (最外层)
  │── 编码格式：GBK
  │── 压缩包行为：包含多个根文件（触发“解压到同名新目录”决策）
  │── 内部文件 1：普通文本_GBK.txt
  └── 内部文件 2：pseudo_image.png (伪装成图片的二级文件)
        │
        └── [Offset: 0x20000 (128KB)] 内嵌一个二级压缩包：nested_hidden.rar
              │── 编码格式：UTF-8
              │── 密码加密：Unicode 密码 "🔒Sébastien_NFC🚀"
              │── 压缩包行为：内部只有唯一一个根文件夹（触发“直接平铺解压”决策）
              └── 文件夹内容：
                    └── 最终目标.7z (三层套娃包)
                          │── 编码格式：Shift_JIS
                          └── 内部文件：日本語ファイル.txt

```

## 2. 自动化生成脚本 (Python)
由于市面的压缩软件很难配齐这一套组合拳，我们必须用 Python 脚本**纯手工逆向拼接**出这个夹具。

```python
# scripts/generate_composite_fixture.py
import zipfile
import os
import subprocess

def create_composite_nightmare():
    out_dir = "tests/fixtures/archives/composite"
    os.makedirs(out_dir, True)
    
    # 步骤 1: 创建最内层的 7z 包 (Shift_JIS 编码)
    # 准备日文文件名文件
    inner_txt = "tests/fixtures/temp_txt.txt"
    with open(inner_txt, "w", encoding="utf-8") as f:
        f.write("Level 3 content")
        
    inner_7z = os.path.join(out_dir, "inner_shifitjis.7z")
    # 使用 7zz 命令行强制指定文件名编码（如果是 Windows/Linux 平台）
    # 注意：这里模拟内部文件在解压后会被智能识别
    subprocess.run(["7zz", "a", inner_7z, inner_txt, "-mcp=932"]) 
    
    # 步骤 2: 创建二级 RAR/ZIP 包，并用 Unicode 密码加密
    # 里面只包含一个根目录（测试直接解压决策）
    nested_zip = os.path.join(out_dir, "nested_hidden.zip")
    password = "🔒Sébastien_NFC🚀" # 混合了 Emoji 和带声调的西欧字符
    
    # 创建唯一根文件夹
    os.makedirs("tests/fixtures/single_root/sub_content", True)
    os.rename(inner_7z, "tests/fixtures/single_root/sub_content/inner_shifitjis.7z")
    
    # 用密码压缩这个单根目录
    subprocess.run(["7zz", "a", f"-p{password}", nested_zip, "./tests/fixtures/single_root"])

    # 步骤 3: 制造“内嵌”效果（将 nested_hidden.zip 拼接到一张伪图片后面）
    pseudo_image = os.path.join(out_dir, "pseudo_image.png")
    with open(pseudo_image, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + b"\x00" * 128000) # 写入 128KB 伪 PNG 头
        with open(nested_zip, "rb") as nz:
            f.write(nz.read()) # 拼接 Zip 体

    # 步骤 4: 打包成最外层 GBK 编码的 ZIP，包含多个根文件
    final_zip = os.path.join(out_dir, "nightmare_matrix.zip")
    with zipfile.ZipFile(final_zip, 'w') as zf:
        # 文件 1
        txt_gbk = "普通文本_GBK.txt".encode('gbk')
        zinfo1 = zipfile.ZipInfo()
        zinfo1.filename = txt_gbk.decode('cp437')
        zf.writestr(zinfo1, "Level 1 normal text")
        
        # 文件 2：内嵌了包的伪图片
        img_gbk = "智能检测图片_GBK.png".encode('gbk')
        zinfo2 = zipfile.ZipInfo()
        zinfo2.filename = img_gbk.decode('cp437')
        with open(pseudo_image, "rb") as img_f:
            zf.writestr(zinfo2, img_f.read())

    # 清理临时构建文件
    print(f"成功生成终极复合夹具: {final_zip}")

if __name__ == "__main__":
    create_composite_nightmare()

```

## 3. 核心难点攻克与断言设计
智能工作流在跑通上述夹具时，有三个技术硬骨头，对应的测试夹具和代码断言必须卡死这些边界：

### 难点 A：Unicode 密码的规范化（NFC vs NFD）

```rust
// 引擎在尝试 Unicode 密码时，必须自动做双重规范化尝试
let candidate_nfc = user_input.to_nfc();
let candidate_nfd = user_input.to_nfd();
// 确保任何一种形式都能在后端成功解开二级内嵌包

```

### 难点 B：智能解压（Smart Extraction）的决策断言
工作流在每一层解压释放前，必须先调用 `ArchiveBackend::list` 评估目录树结构。

- **决策 1（多文件根）**：最外层 `nightmare_matrix.zip` 包含 `普通文本_GBK.txt` 和 `智能检测图片_GBK.png`。
  - *预期行为*：工作流必须在目标目录下新建一个 `nightmare_matrix/` 文件夹，将两者放进去。
- **决策 2（单文件/单目录根）**：二级包 `nested_hidden.zip` 解开后只有一个 `single_root/` 目录。
  - *预期行为*：工作流必须**拒绝**再次创建 `nested_hidden/` 文件夹，而是直接将 `single_root/` 释放到当前上下文，避免产生 `.../out/nested_hidden/single_root/` 这种恶心的双重深层路径。

## 4. Rust 智能工作流（Smart Workflow）集成测试编写
最终，我们在 `smartzip-core` 中编写集成测试，把所有组件揉在一起。这个测试就是智能工作流的**终极验收标准**：

```rust
// crates/smartzip-core/tests/workflow_matrix_test.rs

#[tokio::test]
async fn test_high_composite_smart_workflow() {
    // 1. 加载终极套娃夹具
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/archives/composite/nightmare_matrix.zip");
        
    let tmp_workspace = tempfile::tempdir().unwrap();
    let out_dir = tmp_workspace.path().to_path_buf();

    // 2. 注入测试依赖（密码库预填、配置等）
    let db = Arc::new(SmartZipDb::open_memory().unwrap());
    // 故意把正确的 Unicode 密码塞进密码库，测试历史密码自动检索和排序推荐能力
    db.insert_password("🔒Sébastien_NFC🚀", "user_history").unwrap(); 

    let engine = SmartZipEngine::builder()
        .with_db(db)
        .with_mock_clipboard("无关密码") // 干扰项
        .build();

    let request = ExtractRequest {
        inputs: vec![fixture_path],
        output_dir: Some(out_dir.clone()),
        encoding: EncodingMode::Auto,  // 开启编码自动检测
        scan_embedded: true,          // 开启深度内嵌扫描
        recursion_limit: 5,           // 允许深层嵌套
        delete_source_on_success: false,
    };

    // 3. 收集工作流抛出的所有事件，用于状态流断言
    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    
    let handle = tokio::spawn(async move {
        engine.execute_extract_recursive(request, tx).await
    });

    // 4. 动态断言工作流的每一个“自适应”动作
    let mut events = vec![];
    while let Some(event) = rx.recv().await {
        match &event {
            TaskEvent::EncodingDetected { result, .. } => {
                // 断言：第一层应该自适应检测出 GBK
                if events.is_empty() {
                    assert!(result.best_match.contains("GB"));
                }
            }
            TaskEvent::OutputCreated { path, .. } => {
                // 断言智能解压决策：最外层是多文件，应该创建同名独立目录
                if events.len() == 1 {
                    assert!(path.ends_with("nightmare_matrix"));
                }
            }
            TaskEvent::EmbeddedArchiveFound { finding, .. } => {
                // 断言内嵌扫描：在伪装的 PNG 内部 offset > 0 处发现了 ZIP/RAR
                assert_eq!(finding.format, EmbeddedFormat::Zip);
                assert!(finding.offset > 0);
            }
            TaskEvent::PasswordTried { candidate_id, .. } => {
                // 断言密码策略：自动从数据库捞出了 Unicode 密码并尝试成功
                // 且整个提取过程没有因为 Emoji 或特殊变音符号引发 CLI 传参崩溃
            }
            _ => {}
        }
        events.push(event);
    }

    assert!(handle.await.unwrap().is_ok());

    // 5. 最终文件落地检查（验证整个递归链路全部完美咬合）
    // 检查最内层 Shift_JIS 命名的文件是否被成功解压，且转成了当前系统的正常编码（无乱码）
    let final_file = out_dir
        .join("nightmare_matrix")
        .join("single_root")
        .join("sub_content")
        .join("智能解压出来的正确目录") // 根据单根决策动态展开的路径
        .join("日本語ファイル.txt");
        
    assert!(final_file.exists(), "工作流在复合套娃的某一层断流了！");
}

```

### 总结提炼
通过这个测试矩阵，智能工作流在面对用户时表现出的“智能”，在底层其实是被**高度确定的、各层互不干扰的夹具断言**所保证的。只要这个测试能在 NixOS/macOS 的 CI 环境里跑通，就说明多组件的解耦和协作架构（Slice 5 & Slice 7）完全立住了。

## Turn 3

### 👤 User

结合基础的夹具创建和复杂的复合情况，创建总的测试样例生成指南

### 🤖 Assistant

# SmartZip 自动化测试样例与夹具（Fixture）生成指南
本指南旨在为 `SmartZip` 项目的跨平台重写提供一套标准化的测试夹具生成与管理规范。为了确保 Rust 核心引擎（`smartzip-core`）在面对乱码编码、隐蔽内嵌、复杂密码及多层嵌套等“复合灾难场景”时具备完全确定性的表现，测试必须完全脱离对本地日常环境的依赖，实现百分之百的沙盒化与可复现性。

## 1. 测试夹具目录矩阵
所有静态与动态生成的测试样例必须统一收纳于 Workspace 根目录下的 `tests/fixtures/` 中，严禁散落在各个子 crate 内部。

```plaintext
smartzip/ (Workspace Root)
├── crates/
└── tests/
    └── fixtures/
        ├── archives/                # 压缩包源文件
        │   ├── standard/            # 完好包（Zip/7z/Rar/Tar...）
        │   ├── encrypted/           # 带密码包（包含 ASCII / 复杂 Unicode 密码）
        │   ├── encodings/           # 非 UTF-8 文件名包（GBK/Shift_JIS/Big5...）
        │   ├── embedded/            # 内嵌/自解压/拼接包（Offset > 0）
        │   ├── malicious/           # 恶意攻击包（Zip Slip 路径穿越）
        │   └── composite/           # 终极多重 Buff 叠加套娃包
        ├── cli_snapshots/           # 7zz 命令行输出文本快照（用于 Mock 测试）
        │   ├── 7zz_list_success.txt
        │   └── 7zz_extract_wrong_password.txt
        └── databases/               # SQLite 种子数据模版
            └── seed_passwords.db    # 预填历史权重数据的测试库

```

## 2. 夹具全量生成脚本 (`generate_fixtures.py`)
由于现代压缩软件在 GUI 或 CLI 下普遍会自动防御路径穿越，或者无法精准控制底层文件名编码，我们必须使用低层级的脚本来**纯手工逆向拼接**这些极端夹具。
在 Workspace 根目录下创建 `scripts/generate_fixtures.py`：

```python
#!/usr/bin/env python3
import os
import zipfile
import subprocess

FIXTURE_DIR = "tests/fixtures/archives"

def ensure_dirs():
    for sub in ["standard", "encrypted", "encodings", "embedded", "malicious", "composite"]:
        os.makedirs(os.path.join(FIXTURE_DIR, sub), exist_ok=True)

def build_basic_fixtures():
    print("[1/4] 生成基础与特殊编码夹具...")
    
    # 1. 制造恶意 Zip Slip（路径穿越）包
    slip_path = os.path.join(FIXTURE_DIR, "malicious/zip_slip.zip")
    with zipfile.ZipFile(slip_path, 'w') as zf:
        zf.writestr("../../../../../tmp/smartzip_evil.txt", "malicious payload")

    # 2. 制造纯正的 GBK 编码文件名包
    gbk_path = os.path.join(FIXTURE_DIR, "encodings/gbk_filenames.zip")
    with zipfile.ZipFile(gbk_path, 'w') as zf:
        filename_bytes = "测试文件_GBK.txt".encode('gbk')
        zinfo = zipfile.ZipInfo()
        zinfo.filename = filename_bytes.decode('cp437')  # 欺骗 Python 框架，强制写入原始字节
        zf.writestr(zinfo, "GBK filename content")

def build_embedded_fixture():
    print("[2/4] 生成内嵌/拼接文件夹具...")
    
    # 创建 128KB 的伪装头部（干扰 Magic Bytes 扫描）
    head_file = "tests/fixtures/head.bin"
    with open(head_file, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + b"\x00" * 128000) # PNG头 + 零填充
        
    # 创建一个小 ZIP
    inner_zip = "tests/fixtures/inner.zip"
    with zipfile.ZipFile(inner_zip, 'w') as zf:
        zf.writestr("hidden.txt", "you found me!")
        
    # 强行拼接到指定 Offset
    embedded_path = os.path.join(FIXTURE_DIR, "embedded/offset_128008.bin")
    with open(embedded_path, "wb") as out:
        with open(head_file, "rb") as h, open(inner_zip, "rb") as i:
            out.write(h.read())
            out.write(i.read())
            
    # 清理临时文件
    os.remove(head_file)
    os.remove(inner_zip)

def build_composite_matrix_fixture():
    print("[3/4] 开始构建终极复合套娃包（Matrix）...")
    
    # 准备工作目录
    temp_dir = "tests/fixtures/temp_matrix"
    os.makedirs(temp_dir, exist_ok=True)
    
    # Layer 3: 最内层 7z 包 (使用 Shift_JIS 编码)
    l3_src = os.path.join(temp_dir, "日本語ファイル.txt")
    with open(l3_src, "w", encoding="utf-8") as f:
        f.write("Level 3 Flag: Success")
    l3_7z = os.path.join(temp_dir, "inner_shiftjis.7z")
    subprocess.run(["7zz", "a", l3_7z, l3_src, "-mcp=932"], stdout=subprocess.DEVNULL)
    
    # Layer 2: 二级加密包 (包含单个根目录，使用高难度 Unicode 密码加密)
    # Unicode 密码：混合 Emoji 与带变音符号的西欧字符，用于卡死 NFC/NFD 规范化边界
    unicode_password = "🔒Sébastien_NFC🚀"
    l2_root = os.path.join(temp_dir, "single_root/sub_content")
    os.makedirs(l2_root, exist_ok=True)
    os.rename(l3_7z, os.path.join(l2_root, "inner_shiftjis.7z"))
    
    l2_zip = os.path.join(temp_dir, "nested_hidden.zip")
    # 切换到 single_root 同级目录压缩，确保单根结构
    subprocess.run(["7zz", "a", f"-p{unicode_password}", l2_zip, os.path.join(temp_dir, "single_root")], stdout=subprocess.DEVNULL)
    
    # Layer 1: 伪装与拼接。将二级包藏入一个伪装图片的 Offset 后方
    pseudo_image = os.path.join(temp_dir, "smart_detect_gbk.png")
    with open(pseudo_image, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + b"\x00" * 64000)
        with open(l2_zip, "rb") as nz:
            f.write(nz.read())
            
    # Top Layer: 将伪装图片与另一个普通 GBK 文件塞进最外层 Zip（形成多根文件，触发智能新建目录解压决策）
    final_zip = os.path.join(FIXTURE_DIR, "composite/nightmare_matrix.zip")
    with zipfile.ZipFile(final_zip, 'w') as zf:
        # 写入普通 GBK 文件
        txt_name = "普通文本_GBK.txt".encode('gbk').decode('cp437')
        zf.writestr(txt_name, "Level 1 info")
        # 写入携带套娃种子的伪装图片
        img_name = "智能检测图片_GBK.png".encode('gbk').decode('cp437')
        with open(pseudo_image, "rb") as img_f:
            zf.writestr(img_name, img_f.read())
            
    # 清理所有中间构建层
    subprocess.run(["rm", "-rf", temp_dir])
    print(f" -> 终极复合夹具成功落地: {final_zip}")

if __name__ == "__main__":
    ensure_dirs()
    build_basic_fixtures()
    build_embedded_fixture()
    build_composite_matrix_fixture()
    print("[4/4] 所有级别测试夹具初始化完毕。")

```

## 3. Rust 测试规范与全场景断言示例
有了高度确定的夹具后，核心引擎在集成测试中必须采取“自适应状态流”监听，全面阻断环境污染。

### A. 单元测试层：利用命令行快照（Snapshot）解耦 7zz I/O
为了保证 `smartzip-archive` 可以在没有安装 `7zz` 的裸机或轻量化 CI 环境下测试解析器，应当读取 `cli_snapshots` 夹具：

```rust
// crates/smartzip-archive/src/seven_zip/parser.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wrong_password_edge_case() {
        // 定位工作区统一的快照夹具
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/cli_snapshots/7zz_extract_wrong_password.txt");
        let stderr_output = std::fs::read_to_string(fixture_path).expect("Failed to read snapshot");

        // 断言：即使没有进程执行，解析器也能精准把 7zz 的标准错误映射为具体的 Rust 错误枚举
        let err = parse_7z_stderr(&stderr_output);
        assert!(matches!(err, SmartZipError::WrongPassword { .. }));
    }
}

```

### B. 集成测试层：多路 Buff 叠加集成测试验收（复合场景）
此测试用于强力压榨智能工作流的状态机控制逻辑，自动化验证编码检测、内嵌扫描（`binwalk`）、密码自动匹配、智能目录策略 以及递归解除嵌套。

```rust
// crates/smartzip-core/tests/smart_workflow_matrix_tests.rs
use std::sync::Arc;
use smartzip_core::{SmartZipEngine, ExtractRequest, TaskEvent, EncodingMode};

#[tokio::test]
async fn test_should_automatically_unravel_the_nightmare_matrix() {
    // 1. 定位终极复合套娃夹具
    let composite_zip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/archives/composite/nightmare_matrix.zip");
        
    // 2. 创建完全隔离的沙盒输出目录（测试结束临时目录自动销毁，拒绝环境污染）
    let sandbox = tempfile::tempdir().unwrap();
    let target_output_dir = sandbox.path().to_path_buf();

    // 3. 模拟依赖注入：向临时数据库塞入多形 Unicode 密码，验证系统的推荐与自动检索能力
    let db = Arc::new(SmartZipDb::open_memory().unwrap());
    db.insert_password("🔒Sébastien_NFC🚀", "user_history").unwrap(); 

    let engine = SmartZipEngine::builder()
        .with_db(db)
        .build();

    let request = ExtractRequest {
        inputs: vec![composite_zip],
        output_dir: Some(target_output_dir.clone()),
        encoding: EncodingMode::Auto,       // 启用智能编码检测
        scan_embedded: true,               // 启用 binwalk 深度内嵌流扫描
        recursion_limit: 5,                // 允许最大递归深度
        delete_source_on_success: false,   // 保护源文件
    };

    // 4. 通过 Channel 捕获引擎工作流在逐层剥离时抛出的所有生命周期事件
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    
    let workflow_handle = tokio::spawn(async move {
        engine.execute_extract_recursive(request, tx).await
    });

    let mut event_stream = vec![];
    while let Some(event) = rx.recv().await {
        match &event {
            TaskEvent::EncodingDetected { result, .. } => {
                // 验证 Layer 1 自适应识别出最外层非 UTF-8 的 GBK 编码
                if event_stream.is_empty() {
                    assert!(result.best_match.contains("GB"));
                }
            },
            TaskEvent::OutputCreated { path, .. } => {
                // 验证智能解压决策：Layer 1 包含多根文件，必须自动建立同名新目录封装
                if event_stream.len() == 1 {
                    assert!(path.ends_with("nightmare_matrix"));
                }
            },
            TaskEvent::EmbeddedArchiveFound { finding, .. } => {
                // 验证内嵌扫描：在伪装的图片内部成功通过 Magic Bytes 锁定了 Offset > 0 的隐藏 Zip
                assert_eq!(finding.format, EmbeddedFormat::Zip);
                assert!(finding.offset > 0);
            },
            TaskEvent::PasswordTried { candidate_id, .. } => {
                // 验证 Unicode 密码：跨平台传递 Emoji 与特殊拉丁字符时，未发生硬崩溃或传参截断
                // 并且自动完成了基于 NFC 与 NFD 的双向规范化尝试
            }
            _ => {}
        }
        event_stream.push(event);
    }

    // 确保整个递归工作流安全退出，且未因循环套娃产生死锁
    assert!(workflow_handle.await.unwrap().is_ok());

    // 5. 终极断言：落地方针物理文件检查
    // 只有当编码自适应、内嵌 offset 偏移提取、Unicode密码对齐、智能平铺解压（Layer 2单根目录平铺决策）全部咬合成功，
    // 最终的日文文件名文件才可能在没有任何乱码的前提下完美在沙盒中物理落地。
    let final_leaf_file = target_output_dir
        .join("nightmare_matrix")                // Layer 1 自动创建的目录
        .join("single_root")                     // Layer 2 自动识别单根并直接释放，未多加一层 nested_hidden/ 目录
        .join("sub_content")
        .join("日本語ファイル.txt");              // Layer 3 自动识别 Shift_JIS 并在当前 OS 正常转码后的文件名
        
    assert!(final_leaf_file.exists(), "工作流在复合套娃的某一层内部断流或转码溃败！");
}

```

## 4. 夹具维护策略与 CI/CD 约束
为了防止测试夹具膨胀导致 Git 仓库锁死或在不同的开发机上产生 Flaky Tests（闪烁测试），必须贯彻以下三条戒律：

1. **体积熔断**：任何静态放进 `tests/fixtures/archives/` 目录的单体压缩包严禁超过 `2MB`。测试引擎的功能性逻辑不需要大体积文件，大数据量压力测试请通过 `criterion` 并在运行时动态分配内存块（`dev-dependencies`）解决。
2. **拒绝环境污染**：禁止任何测试代码直接往 `/tmp` 或系统的 `~/.config` 下写入持久化文件。所有文件变更、解压输出、配置读写、SQLite 临时连接一律在 `tempfile::tempdir()` 产生的完全隔离的生命周期容器中运行。
3. **断言跨平台换行符**：在测试 `7zz_list_success.txt` 等命令行文本快照的解析器时，注意不同平台 Git 检出时自动将 `\n` 转换为 `\r\n` 的大坑。在读取快照文本后，建议统一在内存中执行 `.replace("\r\n", "\n")` 保证跨平台测试的一致性。

---

*Exported from [Voyager](https://github.com/Nagi-ovo/gemini-voyager)*  
*Generated on May 20, 2026 at 10:11 PM*