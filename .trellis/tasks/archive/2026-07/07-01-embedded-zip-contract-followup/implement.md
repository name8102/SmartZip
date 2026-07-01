# Embedded and ZIP Contract Follow-up Implementation Plan

## Ordered Steps

1. 审核当前代码覆盖情况
   - 对照新增需求逐项判断：已完成 / 部分完成 / 未完成
   - 记录对应文件、测试和缺口
2. 收敛 embedded 编排缺口
   - `AskUser` 必须真正暂停自动提取
   - `ReportOnly` / `SkipByDefault` 不得继续自动走普通解压路径
   - nested 普通文件扫描必须按 `embedded_scan_mode`、比例阈值和预算执行
3. 收敛 detection/router 缺口
   - archive detection 改成文件头优先
   - extension 仅作 hint/fallback
   - 移除或替代 `SevenZipBackend::probe()` 的完整扫描慢路径
4. 收敛 ZIP 编码闭环
   - 编码检测只对 ZIP 生效
   - 带密码 ZIP 在密码命中后再做 `raw_name` 检测
   - Native ZIP 提供 listing/raw-name 输入
   - CLI 增加编码预览与确认闭环
5. 收敛 CLI 契约
   - 明确手工密码、数据库密码、交互密码和编码确认的顺序
   - 明确 wrong password / partial success 的可见输出
6. 端到端验证
   - embedded fixtures
   - nested fixtures
   - password fixtures
   - encoding fixtures

## Validation

- `cargo test -p smartzip-engine --test embedded_integration` → 9 passed
- `cargo test -p smartzip-engine --test smartzip_integration` → 75 passed
- `cargo test -p smartzip-engine --lib` → 172 passed
- `cargo test -p smartzip-archive --lib` → 51 passed
- `cargo test -p smartzip-cli` → 5 passed

## Non-Goals

- 不重写 `06-27-fix-plan`
- 不恢复 GUI、工作台、打包发布等长期方向
- 不把所有格式都纳入统一编码检测模型

## Exit Condition

- 新增需求已经与旧 `fix-plan` 明确拆分
- 当前代码覆盖情况已完成审计
- 后续实现可以直接按本任务推进而不再回写旧 task

## Audit Summary (2026-07-01)

对照 PRD 的 confirmed follow-up scope，逐项状态如下：

### Embedded orchestration
- **AskUser 真正暂停自动提取** — 已完成
  - `crates/smartzip-engine/src/lib.rs:480-547` 在 `AskUser` 分支发送
    `EmbeddedArchiveSelectionRequired` 事件，无 prompter 直接 skip；有 prompter
    时按 `Extract` / `ExtractAll` / `Skip` 分派。
  - 单测 `tests::embedded_ask_without_prompter_skips_archive` 覆盖。
- **`ReportOnly` / `SkipByDefault` 不再走普通解压路径** — 已完成
  - action 分支的 `_ =>` 默认分支直接 `skipped.push` + `continue`。
- **nested 普通文件扫描遵守模式和预算** — 已完成
  - `should_scan_candidate_for_embedded` 检查 `EmbeddedScanMode::Ignore`、
    `inner_scan_max_bytes` 和 `root_full_scan_confirm_threshold`。
  - `discover_nested_candidates` 中 `nested_embedded_enabled` 只在
    `Aggressive` / `All` 下开启内层普通文件扫描，并按 `inner_scan_max_bytes`
    过滤。
- **business container 接入 ZIP 结构判定** — 已完成
  - 主循环在 root candidate 判定为 ZIP 时先按扩展名映射到
    `BusinessContainerKind`，未命中再调用 `container::classify_zip_path`
    读 central directory。命中时发送 `BusinessContainerSkipped` 并跳过。
  - `discover_nested_candidates` 里同样调用 `classify_zip_path`
    （文件头 / 扩展名两条路径都有）。

### Detection / router
- **文件头优先，extension 仅作 hint/fallback** — 已完成
  - Root candidate 入队时不再预填 `detected_format`。主循环执行
    `probe_file_header` + embedded scan，只有两者都没有结论时才回退到
    `format_from_extension`。
  - `discover_nested_candidates` 中 header-first 逻辑保持不变。
- **`SevenZipBackend::probe()` 移除完整扫描慢路径** — 已完成
  - `crates/smartzip-archive/src/sevenzz.rs` 的 `probe` 改为
    `cheap_probe_format`（首部 512 字节 + 扩展名兜底），不再调用 `7z t`。
  - 测试 `sevenzz::tests::probe_handles_encrypted_archives_without_prompting`
    已更新为对 `encrypted == None` 断言。

### ZIP encoding flow
- **编码检测只对 ZIP 生效** — 已完成
  - `assess_zip_encoding` 的两处调用点都受
    `detected_format == Some(ArchiveFormat::Zip)` 和
    `encoding_mode == EncodingMode::Auto` 两重条件保护。
- **带密码 ZIP 在密码命中后再做 `raw_name` 检测** — 已完成
  - 加密路径在 `test().ok` 之后（或直接 extract 成功之后）才补做
    `zip_encoding_assessment`，避免密码错误时得到无意义的样本。
- **Native ZIP 提供 raw_name 输入** — 已完成
  - `assess_zip_encoding` 用 `NativeZipBackend::list` 取 `entry.raw_name`。
- **CLI 预览 + 确认闭环** — 已完成
  - `StdinEncodingPrompter` / `prompt_encoding_stdin` 提供 accept / manual / skip
    分支；`resolve_encoding_mode` 将结果落成 `EncodingMode::Override` 或
    `SmartZipError::BackendFailed` 触发 skip。

### CLI contract
- **wrong password 可见** — 已完成，`saw_wrong_password` + `TaskEvent::failed`
  + 最终 `extraction_exit_code`。
- **encoding confirmation** — 已完成，见 ZIP encoding 段。
- **password source priority** — 已完成，每次尝试都发
  `Trying password [i/n] (source) …` 进度事件；`ranked_candidates` 按
  数据库排序返回。
- **partial success visible** — 已完成，`extraction_exit_code` 定义了
  0 / 1 / 2 三档退出，CLI 文本和 JSON 输出都会展示 processed / skipped
  计数与逐项 path。

### 结论

本任务列出的 embedded / detection / ZIP encoding / CLI contract 四类新增需求
全部在当前工作树中完成；上述五个测试目标均通过。
