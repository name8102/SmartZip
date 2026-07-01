# 内嵌归档检测策略

> 状态：专题设计研究  
> 说明：本文描述内嵌归档检测的目标策略。部分内容已实现，部分仍是待落地设计；当前进度请结合 `docs/implementation-progress.md` 和相关 `.trellis/tasks/` 阅读。
> 基于 2026-06-12 确认的产品决策

## 1. 场景定义

### 1.1 改后缀伪装（DirectArchiveDisguised）

文件扩展名不是归档，但 offset=0 处是归档 magic bytes。

```text
示例：
  R4047.jpg  -> ZIP header at offset 0
  movie.mp4  -> RAR5 header at offset 0
  setup.exe  -> 7z header at offset 0
```

处理：直接按普通归档解压，不做业务容器排除。

### 1.2 前缀伪装 / PrependedCarrier

文件 offset=0 是媒体/可执行文件头，archive magic at offset > 0，且压缩段占主体积（>= 70%）。

```text
示例：
  photo.jpg (904331 bytes) + RAR5 (596178986 bytes) = R4047.jpg
  video.mp4 header + ZIP payload =伪装视频
  EXE stub + 7z payload = SFX-like
```

处理：carve 后解压。ZIP 额外尝试 EOCD 修正尾部边界。

### 1.3 普通 embedded payload

archive magic at offset > 0，但压缩段低于 dominant 阈值（< 70%）。

```text
示例：
  PDF 内嵌 ZIP 附件
  EXE 资源段中的小 ZIP
  日志文件中间夹带的压缩段
```

处理：GUI 询问；CLI 非交互默认报告并跳过。

### 1.4 多 payload（MultiPayload）

文件中发现多个独立的 archive findings。

处理：
- 只有最大 finding 占总文件 >= 70% 时才自动选最大
- 否则要求用户选择

### 1.5 SFX / 自解压包

EXE/DMG 等可执行文件内嵌归档。

处理：
- root：自动扫描
- inner：默认不扫描，aggressive/all 才扫

### 1.6 业务容器（BusinessContainer）

ZIP-family 格式的文档/应用容器。

处理：
- root：用户直接选中则处理，不做排除
- inner：默认不展开

## 2. DetectionKind / DetectionAction

```rust
enum DetectionKind {
    DirectArchive,            // offset = 0
    DirectArchiveDisguised,   // offset = 0，但扩展名不是归档
    PrependedCarrier,         // offset > 0 且占主体积
    EmbeddedPayload,          // offset > 0 但低于 dominant 阈值
    MultiPayload,
    BusinessContainer,
    NotArchive,
    Ambiguous,
}

enum DetectionAction {
    ExtractDirect,
    CarveAndExtract,
    AskUser,
    SkipByDefault,
    ReportOnly,
}
```

分类映射：

| DetectionKind | DetectionAction |
|---------------|-----------------|
| DirectArchive | ExtractDirect |
| DirectArchiveDisguised | ExtractDirect |
| PrependedCarrier | CarveAndExtract |
| EmbeddedPayload | AskUser (GUI) / ReportOnly (CLI) |
| MultiPayload | AskUser |
| BusinessContainer | SkipByDefault (inner) / ExtractDirect (root) |
| NotArchive | SkipByDefault |
| Ambiguous | AskUser |

## 3. RootInput 检测流程

```text
1. 分卷识别
   - .part1.rar / .7z.001 / .001 等
   - 分卷命名只是候选，必须由 header/probe 确认
   - 非首卷跳过

2. header/probe 检测 offset=0 归档
   - 成功：DirectArchive / DirectArchiveDisguised
   - 不做业务容器排除
   - 直接解压

3. 如果 offset=0 不是归档：
   - 文件 <= 10GB：默认全文件 binwalk scan
   - 文件 > 10GB：请求用户确认后全文件 scan

4. finding 分类
   - offset = 0：
       DirectArchive
   - offset > 0 且 archive_size / file_size >= 70%：
       PrependedCarrier
   - offset > 0 且 ratio < 70%：
       EmbeddedPayload
   - 多 finding：
       只有最大 finding 占总文件 >= 70% 才自动选最大
       否则要求用户选择

5. PrependedCarrier：
   - 一律 carve，不直接把原文件交给 backend
   - size=None 时切 offset..EOF
   - ZIP 尝试 EOCD 修正真实结束位置
   - carve 后按普通归档进入 list/test/extract

6. EmbeddedPayload：
   - GUI 询问
   - CLI 非交互默认报告并跳过
```

## 4. ExtractedFile 检测流程

```text
1. 分卷识别
   - 分卷命名只是候选
   - header/probe 必须确认

2. header/probe 检测 offset=0 归档
   - 后缀不作为必要条件
   - 文件名 abc，只要文件头是 ZIP/RAR/7Z，也继续解压

3. ZIP-family 业务容器判断
   - 只作用于内层文件
   - docx/xlsx/pptx/epub/apk/jar/cbz/cbr 默认不展开
   - 不是业务容器结构的 ZIP，即使后缀伪装成 .docx，也按普通 ZIP 解压

4. 非 offset=0 归档：
   - auto 模式默认跳过
   - aggressive 模式扫描 dominant embedded archive
   - all 模式扫描并入队所有合格 finding
```

## 5. Dominant Finding Selection

```text
dominant_min_ratio = 0.70 (默认，可配置)

单 finding:
  - offset = 0 -> DirectArchive
  - offset > 0 且 ratio >= 0.70 -> PrependedCarrier
  - offset > 0 且 ratio < 0.70 -> EmbeddedPayload

多 finding:
  - 最大 finding ratio >= 0.70 且其他 finding 总和 < 10% -> 自动选最大
  - 否则要求用户选择
```

## 6. Business Container Classifier

仅作用于内层文件。通过内部结构判断，不依赖后缀。

```text
ZIP header 检测到时，list 中央目录前若干 entry：

docx:
  [Content_Types].xml
  word/document.xml

xlsx:
  [Content_Types].xml
  xl/workbook.xml

pptx:
  [Content_Types].xml
  ppt/presentation.xml

epub:
  mimetype = application/epub+zip
  META-INF/container.xml

apk:
  AndroidManifest.xml
  classes.dex 或 resources.arsc

jar:
  META-INF/MANIFEST.MF
  .class 文件

cbz/cbr:
  主要 entry 是图片 (jpg/png/webp)
```

复杂度可控：只需要 `list`，不需要解压内容。

## 7. Carve / Materialize 策略

### 7.1 ZIP EOCD End Detection

```text
ZIP embedded finding 处理：
  1. 如果 binwalk 给出 size，用 offset..offset+size carve
  2. 如果 size=None，先切 offset..EOF
  3. 对 ZIP 额外尝试解析 EOCD，计算真实 ZIP end
  4. 如果发现 EOCD 后还有尾部垃圾，则优先二次裁剪到 EOCD end
  5. native zip 失败时，再 fallback 7zz
```

### 7.2 其他格式

| 格式 | 尾部判断 | MVP 策略 |
|------|----------|----------|
| ZIP | 可以，解析 EOCD / ZIP64 EOCD | 实现 EOCD 修正 |
| 7z | 可以，但要解析 7z header | 依赖 binwalk size |
| RAR5 | 可以，但实现复杂度较高 | 依赖 binwalk size / unrar |
| gzip/xz/bzip2 | 可部分判断 | 多成员流、尾部垃圾语义要谨慎 |
| tar | 难度低 | 判断连续 zero block 后是否还有数据 |

MVP 只做：ZIP EOCD end detection + binwalk size 优先 + backend test/list 验证。

### 7.3 Carve 后处理

```text
carve 后：
  1. 写入临时文件
  2. backend probe/list/test 验证
  3. 验证通过 -> 进入正常解压流程
  4. 验证失败 -> 报错 EmbeddedArchiveCarveFailed
```

## 8. CLI / GUI 行为

### 8.1 CLI 命令

```bash
# 提取模式
smartzip extract <paths...> --embedded auto
smartzip extract <paths...> --embedded ask
smartzip extract <paths...> --embedded largest
smartzip extract <paths...> --embedded aggressive
smartzip extract <paths...> --embedded all
smartzip extract <paths...> --embedded ignore

# 参数
smartzip extract <paths...> --dominant-min-ratio 0.70
smartzip extract <paths...> --confirm-root-scan-over 10GiB

# 检测命令
smartzip detect <path> --json
smartzip detect <path> --deep
smartzip detect <path> --max-scan-bytes 0
```

兼容别名：
- `--deep` 映射到 `--embedded aggressive`
- `--scan-embedded` 映射到 `--embedded auto`

### 8.2 detect --json 输出

```json
{
  "path": "R4047.jpg",
  "classification": "prepended_carrier",
  "action": "carve_and_extract",
  "format": "rar5",
  "offset": 904331,
  "size": 596178986,
  "archive_ratio": 0.9985,
  "requires_user_confirmation": false
}
```

### 8.3 GUI 展示

伪装承载压缩包（PrependedCarrier）：

```text
检测到伪装承载压缩包：
- 分类：PrependedCarrier
- 格式：RAR5
- 偏移：904331
- 大小：596178986
- 占比：99.85%
- 操作：切片后解压

[解压] [查看检测详情] [取消]
```

低于阈值的 root embedded payload：

```text
检测到文件中包含小型压缩段，但未达到自动处理阈值。
是否提取该片段？
```

多个 finding：

```text
检测到多个压缩段，请选择要提取的片段。
```

## 9. 测试矩阵

### 9.1 RootInput 测试用例

| 场景 | 输入 | 预期行为 |
|------|------|----------|
| 改后缀 ZIP | file.jpg (ZIP at offset 0) | DirectArchiveDisguised, 直接解压 |
| PrependedCarrier RAR | photo.jpg + RAR5 (ratio=99.85%) | Carve and extract |
| 低 ratio payload | PDF with small ZIP attachment | Ask/Report |
| 多 finding dominant | 大 ZIP + 小 ZIP (ratio=85%) | 自动选最大 |
| 多 finding ambiguous | 两个中等 ZIP (ratio=40%+35%) | 要求用户选择 |
| >10GB file | 需要确认 | RootFullScanConfirmationRequired |
| 分卷伪装 | .part1.rar as .jpg | 识别分卷并处理 |

### 9.2 ExtractedFile 测试用例

| 场景 | 输入 | 预期行为 |
|------|------|----------|
| 内层 ZIP | archive.zip 内有 nested.zip | 自动入队 |
| 内层 docx | archive.zip 内有 report.docx | SkipByDefault (auto) |
| 内层 APK | archive.zip 内有 app.apk | SkipByDefault (auto) |
| aggressive 模式 | 内层普通文件有 dominant ZIP | 扫描并处理 |
| all 模式 | 内层普通文件有多个 ZIP | 扫描并入队所有 |

### 9.3 Business Container 测试用例

| 格式 | 内部结构 | 预期判断 |
|------|----------|----------|
| docx | [Content_Types].xml + word/document.xml | BusinessContainer |
| xlsx | [Content_Types].xml + xl/workbook.xml | BusinessContainer |
| epub | mimetype + META-INF/container.xml | BusinessContainer |
| apk | AndroidManifest.xml + classes.dex | BusinessContainer |
| jar | META-INF/MANIFEST.MF | BusinessContainer |
| cbz | 主要 entry 是图片 | BusinessContainer |
| 普通 ZIP | 无业务容器特征 | 不是 BusinessContainer |

### 9.4 ZIP EOCD 测试用例

| 场景 | 输入 | 预期行为 |
|------|------|----------|
| 干净 ZIP | ZIP without trailing data | 正常解压 |
| 尾部垃圾 | ZIP + 1MB garbage | EOCD 修正后解压 |
| 无 binwalk size | size=None, offset..EOF | 尝试 EOCD 修正 |
| EOCD 损坏 | corrupted EOCD | fallback 7zz 或报错 |
