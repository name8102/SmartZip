# binwalk crate 评估

> 目标：评估 `binwalk` 是否适合作为 SmartZip `smartzip-scanner` 的内嵌压缩包检测实现  
> 版本：`binwalk = 3.1.0`  
> 来源：crates.io、docs.rs、本地 Cargo 源码

## 1. 结论

`binwalk` 很适合 SmartZip 的 **文件内嵌压缩包检测** 需求。

建议：

```text
MVP scanner = SmartZip wrapper + binwalk::Binwalk::scan()
```

使用边界：

- ✅ 用于检测文件中是否存在内嵌压缩包/文件结构。
- ✅ 用于获得 offset、size、format、confidence、description。
- ✅ 用于 `smartzip detect` 和 GUI 提示。
- ⚠️ MVP 不直接使用 binwalk extraction，提取仍交给 `ArchiveBackend` / 7zz。
- ⚠️ 大文件必须限制读取大小，因为 `Binwalk::scan(&[u8])` 接收内存字节切片。

## 2. binwalk API 摘要

README 示例：

```rust
use binwalk::Binwalk;

let binwalker = Binwalk::new();
let file_data = std::fs::read("/tmp/firmware.bin")?;

for result in binwalker.scan(&file_data) {
    println!("{:#?}", result);
}
```

主要类型：

```rust
pub struct Binwalk {
    pub signature_count: usize,
    pub short_signatures: Vec<Signature>,
    pub patterns: Vec<Vec<u8>>,
    pub pattern_signature_table: HashMap<usize, Signature>,
    pub extractor_lookup_table: HashMap<String, Option<Extractor>>,
}
```

```rust
pub struct SignatureResult {
    pub offset: usize,
    pub id: String,
    pub size: usize,
    pub name: String,
    pub confidence: u8,
    pub description: String,
    pub always_display: bool,
    pub extraction_declined: bool,
    pub preferred_extractor: Option<Extractor>,
}
```

主要方法：

```rust
impl Binwalk {
    pub fn new() -> Binwalk;

    pub fn configure(
        target_file_name: Option<String>,
        output_directory: Option<String>,
        include: Option<Vec<String>>,
        exclude: Option<Vec<String>>,
        signatures: Option<Vec<Signature>>,
        full_search: bool,
    ) -> Result<Binwalk, BinwalkError>;

    pub fn scan(&self, file_data: &[u8]) -> Vec<SignatureResult>;
    pub fn extract(...);
    pub fn analyze(...);
}
```

## 3. 适配 SmartZip 的价值

### 3.1 直接覆盖“文件内嵌压缩包检测”

SmartZip 需要识别：

- 自解压包
- 伪装后缀文件
- 拼接文件中的 zip/7z/rar/gzip 等压缩段
- 文件真实格式不依赖扩展名

`binwalk` 的定位正是：

> Analyzes data for embedded file types

### 3.2 签名覆盖广

源码中已确认存在以下 SmartZip 相关 signatures / extractors：

- `zip`
- `sevenzip`
- `rar`
- `gzip`
- `bzip2`
- `xz`
- `tarball`
- `cab`
- `dmg`
- `iso9660`
- `zstd`
- `lz4`
- `lzma`

除此之外还有大量固件、文件系统、镜像、PE/ELF 等签名。SmartZip 应通过 include/白名单只保留关心的压缩类结果。

### 3.3 扫描算法适合多签名检测

`Binwalk::scan` 内部使用 Aho-Corasick：

```rust
let grep = AhoCorasick::new(self.patterns.clone()).unwrap();
for magic_match in grep.find_overlapping_iter(&file_data[next_valid_offset..]) {
    ...
}
```

这比我们手写多个 magic bytes 扫描更成熟。

### 3.4 parser 会验证结构并推导大小

例如：

- zip parser 会解析 ZIP header，并查找 EOCD，推导 `size` 和 file count。
- 7z parser 会解析 7z header，并校验 next header CRC。
- rar parser 会解析 RAR header，并尝试查找 EOF marker。

这比单纯搜索 magic bytes 更可靠。

## 4. 风险与限制

### 4.1 内存读取限制

`scan` 签名为：

```rust
pub fn scan(&self, file_data: &[u8]) -> Vec<SignatureResult>
```

这意味着 SmartZip 需要先把待扫描数据读入内存。

缓解：

- fast mode 只读头尾或限定大小。
- deep mode 才读完整文件。
- 设置 `max_scan_bytes`。
- 对超过限制的文件提示用户手动深度扫描。

### 4.2 结果范围过宽

binwalk 面向固件分析，会返回很多 SmartZip 不需要的格式。

缓解：

- 使用 `Binwalk::configure(..., include, exclude, ...)`。
- SmartZip 再做二次白名单过滤。
- 默认只保留压缩/归档/镜像类：zip、7z、rar、gzip、bzip2、xz、tarball、cab、dmg、iso 等。

### 4.3 extraction 不作为 MVP 直接能力

binwalk 有 `extract` 和 `analyze(do_extraction=true)`，但 SmartZip 不应直接让 scanner 执行提取。

原因：

- SmartZip 已有 `ArchiveBackend` 安全边界。
- 解压需要统一日志、任务事件、密码策略、路径安全检查。
- binwalk extraction 面向固件拆包，不等价于 SmartZip 用户解压体验。

因此 MVP 只使用 scan；提取由 `7zz` 或后续 library backend 完成。

## 5. SmartZip 封装设计

```rust
pub struct EmbeddedScanner {
    binwalk: binwalk::Binwalk,
    config: ScannerConfig,
}

pub struct ScannerConfig {
    pub mode: ScanMode,
    pub max_scan_bytes: Option<u64>,
    pub max_findings: usize,
    pub include_formats: Vec<EmbeddedFormat>,
    pub min_confidence: Confidence,
}

pub enum ScanMode {
    Fast,
    Deep,
}

pub enum EmbeddedFormat {
    Zip,
    SevenZip,
    Rar,
    Gzip,
    Bzip2,
    Xz,
    Tar,
    Cab,
    Dmg,
    Iso,
    Other(String),
}

pub struct EmbeddedArchiveFinding {
    pub offset: u64,
    pub size: Option<u64>,
    pub format: EmbeddedFormat,
    pub confidence: Confidence,
    pub description: String,
}
```

映射规则：

```text
SignatureResult.name        → EmbeddedFormat
SignatureResult.offset      → offset
SignatureResult.size == 0   → None
SignatureResult.confidence  → Confidence enum
SignatureResult.description → description
```

## 6. CLI / GUI 行为

### CLI

```bash
smartzip detect file.bin
smartzip detect file.bin --deep
smartzip detect file.bin --json
```

输出示例：

```text
Found embedded archive:
- format: zip
  offset: 1048576
  size: 204800
  confidence: high
  description: ZIP archive, file count: 12, total size: 204800 bytes
```

### GUI

检测到内嵌压缩包时提示：

```text
检测到文件内可能包含压缩包：
- ZIP archive @ 0x100000，大小 200 KB，置信度 high

[提取内嵌压缩包] [继续普通解压] [忽略]
```

## 7. 设计更新

已同步更新：

- `docs/design.md`

变更点：

1. `smartzip-scanner` 明确使用 `binwalk` crate 作为 MVP 主实现。
2. Slice 7 改为“引入 binwalk 并封装 `EmbeddedScanner`”。
3. 风险表增加“使用 binwalk 时限制读取大小”。

## 8. 来源

- [binwalk crate on crates.io](https://crates.io/crates/binwalk)
- [binwalk repository](https://github.com/ReFirmLabs/binwalk)
