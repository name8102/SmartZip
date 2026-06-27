# Issue Inventory

## Goal

把当前“功能实现存在很多问题”的描述整理为结构化问题清单，确保后续修复任务基于证据而不是基于印象推进。

本任务在问题梳理前，先承接旧 `docs/implementation-progress.md` 和 `docs/implementation-plan.md` 中与问题相关的历史声明，避免后续只有“现在坏了”而没有“原来打算做到什么”的参照。

## Confirmed Facts

- 父任务已确认当前已知问题主要集中在解压、分卷、密码、编码、环境适配和递归限制测试。
- `docs/implementation-progress.md` 记录了多阶段实现历史，但不等于当前真实状态，需要重新核实后再引用。
- `cargo test -q test_engine_respects_recursion_limit -- --nocapture` 在当前环境下通过，因此该问题不能直接作为已确认活跃故障。
- 旧进度文档中存在多处“已完成/已可用”表述，当前只能视为历史实现声明，不能直接等价为产品可用性结论。

## Requirements

- 问题清单必须按类别分组，至少覆盖：
  - 解压成功率
  - 分卷处理
  - 加密包/密码流程
  - ZIP 编码
  - 批量解压与嵌套解压流程稳定性
  - CLI 交互与多输入可用性
  - 文件操作、覆盖/删除与目标路径选择
  - 后端环境与外部命令定位
  - 回归测试与行为一致性
- 每个问题条目都必须包含统一字段：
  - 现象
  - 迁移来源
  - 证据来源
  - 影响范围
  - 可能根因
  - 优先级
  - 建议验证方式
- 对来自旧进度文档的“已完成”描述，必须额外标注状态分类：
  - 当前已证实成立
  - 当前待验证
  - 已被用户反馈或代码现状推翻
- 仅记录已核实或可复现的问题；纯猜测需要明确标记为待验证。
- 问题清单应区分“立即修复的真实缺陷”和“后续能力建设缺口”。

## Initial Inventory Seed

## Migrated Historical Claims To Reclassify

以下条目来自旧 `docs/implementation-progress.md` 或 `docs/implementation-plan.md`，迁移到 Trellis 后不能直接保留为“已完成”，需要重新归类：

- “后端路由已完成，zip/7z/rar/tar/gz/bz2 可用”
  - 当前状态：待按真实样本和 CLI 行为验证
- “编码自动检测已完成”
  - 当前状态：已被用户反馈推翻为当前不可用或至少不可视为完成
- “智能解压核心已完成”
  - 当前状态：待拆成批量、分卷、密码、嵌套、输出路径等子能力逐项核实
- “extract 命令已可用”
  - 当前状态：仅能视为命令存在，不等于近期验收标准已满足
- “后处理规则部分完成”
  - 当前状态：与当前用户反馈冲突，需重点核实覆盖/删除/目标路径行为
- “smart output layout / temp dir 安全提取 已实现”
  - 当前状态：仅能视为实现声明，仍需对照用户反馈验证其是否满足目标路径、覆盖和删除设计
- “GUI 是下一步 P0”
  - 当前状态：已不符合当前优先级，应降级，不再干扰当前问题排序

### P0 Candidate: 解压成功率不稳定

- 现象
  - 用户反馈“只有部分压缩包能正常解压”。
- 迁移来源
  - `docs/implementation-progress.md` 将 `extract`、后端路由和智能解压核心多次记为已完成或可用。
- 证据来源
  - 父任务用户描述。
  - 后端路径当前涉及 `NativeZipBackend`、`UnrarBackend`、`SevenZipBackend` 和 `BackendRouter`，实现分散在 `crates/smartzip-archive/src/native_zip.rs`、`unrar.rs`、`sevenzz.rs`、`router.rs`。
- 影响范围
  - 直接影响主命令 `extract` 的可用性。
- 可能根因
  - 多后端 fallback 行为不一致，或错误类型映射不稳定。
- 状态
  - 需要基于失败样本复现。

### P0 Candidate: 分卷包处理失败

- 现象
  - 用户反馈分卷压缩包经常失败。
- 迁移来源
  - `docs/requirements.md` 明确把分卷首卷识别和非首卷跳过列为必须能力。
  - `docs/implementation-progress.md` 把“分卷跳过”记为已完成。
- 证据来源
  - 父任务用户描述。
  - 路由与首卷判断逻辑位于 `crates/smartzip-archive/src/router.rs` 与 `crates/smartzip-engine/src/lib.rs`。
- 影响范围
  - RAR/7z 分卷场景，属于 SmartZip 重点能力。
- 可能根因
  - 首卷识别、缺卷诊断、具体后端能力之间存在断层。
- 状态
  - 需要补最小复现用例。

### P0 Candidate: 正确密码仍解压失败

- 现象
  - 用户反馈加密包在密码正确时有时仍失败。
- 迁移来源
  - `docs/requirements.md` 要求能区分密码错误、损坏、格式不支持，并支持自动尝试与交互补输。
  - `docs/implementation-plan.md` 曾把密码流程稳定性列入早期修复目标。
- 证据来源
  - 父任务用户描述。
  - engine 中密码测试/提取流程位于 `crates/smartzip-engine/src/lib.rs`，存在 `WrongPassword` 分支与交互重试路径。
- 影响范围
  - 加密归档主流程。
- 可能根因
  - `test -> extract` 两阶段行为、编码参与时机、fallback 后端差异。
- 状态
  - 需要结合真实 fixture 复现。

### P1 Candidate: ZIP 编码路径复杂且需重新定界

- 现象
  - 旧问题描述为“编码检测不可用”；当前代码中虽有编码处理逻辑，但用户反馈其仍不可用，且适用范围应主要限于 ZIP。
- 迁移来源
  - `docs/requirements.md` 把自动编码检测写成默认能力。
  - `docs/implementation-progress.md` 把编码检测接入 engine 记为已完成。
- 证据来源
  - `crates/smartzip-engine/src/lib.rs` 中 `EncodingMode::Auto/Override` 分支。
  - `crates/smartzip-archive/src/native_zip.rs` 中原始文件名字节解码与测试。
  - `crates/smartzip-archive/src/sevenzz.rs` 中 code page 映射。
- 影响范围
  - ZIP 乱码文件名场景。
- 可能根因
  - 需求认知与当前实现、不同后端能力边界未完全对齐。
- 状态
  - 需要区分“当前 bug”与“能力边界”。

### P0 Candidate: 批量解压和嵌套解压流程不稳定

- 现象
  - 用户反馈批量解压和嵌套解压“感觉不够稳”，需要排查流程设计和实现。
- 迁移来源
  - `docs/requirements.md`、`docs/design.md` 都把批量任务隔离、递归/动态节点、失败不互相阻塞作为目标行为。
  - `docs/implementation-progress.md` 对递归队列、嵌套入队、批次流程有较强“已完成”表述。
- 证据来源
  - 用户反馈。
  - 旧设计/进度文档。
- 影响范围
  - 核心 `extract` 主流程，尤其多输入、多层嵌套场景。
- 可能根因
  - 当前 engine 工作流设计、状态推进、错误隔离和后处理时序未真正满足文档契约。
- 状态
  - 需要基于真实样本和 CLI 行为补复现。

### P1 Candidate: CLI 批量交互设计不够易用

- 现象
  - 用户反馈批量解压需要多个输入时交互混乱，CLI 设计不够易用。
- 迁移来源
  - `docs/requirements.md` 已将 CLI 作为第一版核心入口，并要求行为清晰、退出码明确。
  - `docs/design.md` 要求 CLI 与核心引擎共享同一套行为，不应因为交互设计导致体验分叉。
- 证据来源
  - 用户反馈。
- 影响范围
  - 当前主交付入口 `smartzip extract <paths...>`。
- 可能根因
  - 多输入任务模型、密码补输流程、输出路径/确认策略与 CLI 展示未统一。
- 状态
  - 待结合实际命令路径确认。

### P0 Candidate: 文件操作和目标路径行为与设计不符

- 现象
  - 用户反馈删除、覆盖处理不够完善，智能解压目标路径选择与设计不符。
- 迁移来源
  - `docs/requirements.md` 对单文件/多文件输出目录、冲突命名、删除源文件默认关闭、安全删除均有明确要求。
  - `docs/design.md` 对事务式输出、提交阶段、默认不覆盖也有明确描述。
  - `docs/implementation-progress.md` 对“智能输出结构”“temp dir 安全提取”“后处理规则部分完成”有历史声明。
- 证据来源
  - 用户反馈。
  - 旧需求/设计文档。
- 影响范围
  - 所有解压成功后的落盘与后处理路径。
- 可能根因
  - 智能输出布局、提交/覆盖策略、删除时机与设计收敛不足。
- 状态
  - 需要对照 CLI 行为和 engine/materialize 路径验证。

### P0 Candidate: 外部依赖环境与后端定位不匹配

- 现象
  - 当前机器存在 `/usr/bin/7z` 和 `/usr/bin/unrar`，不存在 `7zz` 和 `rar`。
- 迁移来源
  - `docs/design.md` 与 `docs/implementation-plan.md` 都依赖 format-aware router 和外部工具 fallback。
- 证据来源
  - `command -v 7z; command -v 7zz; command -v unrar; command -v rar`
  - `SevenZipLocator` 候选为 `7zz`, `7z`；`UnrarLocator` 候选为 `unrar`。
- 影响范围
  - 所有依赖外部命令的集成测试和真实提取流程。
- 可能根因
  - 环境假设与本机实际安装不一致，错误信息或 fallback 诊断可能不清晰。
- 状态
  - 已核实，应纳入近期检查。

### Historical Report: 递归限制回归

- 现象
  - 先前规划笔记认为 `test_engine_respects_recursion_limit` 失败。
- 证据来源
  - 当前单测重跑通过；测试定义位于 `crates/smartzip-engine/tests/smartzip_integration.rs:587`。
- 影响范围
  - 当前不应视为活跃故障，除非全量测试再次复现。
- 状态
  - 暂降为历史报告，待后续全量验证。

## Migrated Evidence Gaps

以下旧文档承诺已经迁入 Trellis，但当前仍缺直接证据，后续必须补：

- CLI 退出码是否已能稳定区分部分成功、密码错误、损坏包、安全失败
- 目标路径策略是否真能满足“单项直落 / 多项建目录 / 默认不覆盖”
- 安全预算与同盘临时目录是否在当前主路径真正生效
- 根输入 magic bytes 扫描与内层默认保守扫描是否符合设计边界
- 默认密码智能模式与后续深度模式是否已形成可解释契约

## Acceptance Criteria

- [ ] 形成一份按类别组织的问题清单模板，可直接供修复计划引用。
- [ ] 问题清单已经吸收旧进度文档里的历史完成声明，并标出其当前状态，而不是只记录新的口头反馈。
- [ ] 已知问题均补上证据字段，而不是只保留口头描述。
- [ ] 至少明确哪些问题属于 P0/P1 立即修复范围。
- [ ] 至少一个回归测试问题被明确纳入清单并给出验证命令或测试名。
- [ ] 明确区分“已核实问题”“待复现问题”“历史报告不再作为当前 blocker”。
