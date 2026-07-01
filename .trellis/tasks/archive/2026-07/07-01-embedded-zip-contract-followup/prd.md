# Embedded and ZIP Contract Follow-up

## Goal

承接从 `06-27-fix-plan` 中拆出的新增需求，把内嵌归档编排、ZIP 编码确认流程、文件头优先检测、`SevenZipBackend::probe()` 慢路径，以及相关 CLI 契约问题收敛为一个独立的后续修复任务。

本任务存在的原因是：这些内容是在 `fix-plan` 已完成之后追加进去的，已经超出了原任务“整理修复路线”的完成边界，因此必须拆分为新的实现/验证任务，而不是回写成旧任务未完成。

## Split Boundary

- `06-27-fix-plan` 保持已完成，继续代表旧计划迁移后的阶段化修复路线。
- 本任务承接后来新增的主流程契约问题，不追溯修改 `fix-plan` 的完成结论。
- 本任务默认以现有工作树为起点，检查哪些代码已经覆盖这些需求，哪些仍未完成。

## Confirmed Follow-up Scope

- 内嵌归档流程新增确认问题：
  - `AskUser` 分支实际上继续自动解压，没有真正暂停等待调用方决定
  - `embedded_scan_mode` / `dominant_min_ratio` / `confirm_large_scan` 已暴露到 CLI，但 engine 没有按这些参数驱动行为
  - 内层普通文件的 embedded 扫描近似 `aggressive/all`，没有按模式和预算约束
  - business container 当前只按扩展名跳过，没有接入 ZIP 结构判定
- 检测与路由新增确认问题：
  - root / nested candidate 大量依赖扩展名，缺少文件头优先和 `probe` fallback
  - `SevenZipBackend::probe()` 仍通过 `7z t` 判定支持性，存在完整扫描慢路径
- ZIP 编码流程新增确认问题：
  - 编码检测被错误应用到非 ZIP 格式
  - 带密码 ZIP 的编码检测时机不对，应该在命中正确密码之后
  - `7z` 后端无法提供 ZIP 原始文件名字节，必须与 Native ZIP 的 listing 能力组合使用
  - `EncodingDetected` 事件已接入 engine/CLI，但这不等于编码流程已形成可用闭环
- CLI 契约新增确认问题：
  - CLI 目前只展示编码检测事件，没有交互确认闭环
  - 需要明确“手工密码”“数据库密码”“编码确认”三者的用户可见顺序与覆盖语义

## Requirements

- 任务必须明确哪些新增需求已经由当前代码覆盖，哪些仍待实现。
- 任务必须把 embedded、detection/router、ZIP encoding、CLI contract 四条线分别列出目标、非目标和验证方式。
- 任务必须要求 ZIP 编码流程收敛到：
  - 文件头确认 ZIP
  - 密码命中
  - Native ZIP listing 读取 `raw_name`
  - CLI 预览/确认编码
  - 再用确认后的编码进入提取
- 任务必须要求 embedded 流程收敛到：
  - `AskUser` 真正暂停自动提取
  - `ReportOnly` / `SkipByDefault` 不沿用普通解压路径
  - nested 普通文件扫描遵守模式和预算
- 任务必须把 `SevenZipBackend::probe()` 慢路径是否移除列为显式验收项。

## Acceptance Criteria

- [x] 已将 later-added embedded / ZIP / CLI 契约需求从 `06-27-fix-plan` 拆分为独立 task。
- [x] 新 task 明确区分 embedded、detection/router、ZIP encoding、CLI contract 四类工作。
- [x] 文档明确 `fix-plan` 已完成，而这些内容属于后续新增范围。
- [x] 每类工作至少包含一种可执行验证方式。
- [x] 缺口补全：root candidate 采用 header-first 检测；extension 仅作 hint/fallback。
- [x] 缺口补全：root candidate 通过扩展名 + `classify_zip_path` 双通道判定 business container，并发送 `BusinessContainerSkipped` 事件跳过。
