# SmartZip 文档迁移设计

## Objective

将项目的规划类文档从“`docs/` 散落文件 + 已偏离现状的流程说明”收敛为“父任务总览 + 子任务分工 + Trellis 作为单一事实来源”。

迁移不是简单复制。设计上分三层：

1. 文档迁移层：先把旧文档里的需求、设计意图、阶段计划和历史进度声明拆分到 Trellis 对应任务。
2. 现实校正层：再根据当前程序行为把内容归类为“仍成立”“需修正”“仅保留历史参考”。
3. 调查回灌层：把代码/测试/样本调查得到的现实证据重新回灌到问题清单和修复计划，避免修复顺序继续被历史叙述主导。

## Source Of Truth

- 任务级规划：`.trellis/tasks/<task>/prd.md`、`design.md`、`implement.md`
- 长期开发规范：`.trellis/spec/<package>/<layer>/...`
- 历史参考文档：`docs/` 中原有规划文件，仅作为迁移输入或归档材料

本轮完成后，任何新的需求、问题清单、修复路线都先写入 Trellis 任务文档，不再把 `docs/requirements.md`、`docs/design.md`、`docs/implementation-plan.md` 作为继续演进的工作文件。

## Migration Mapping

### 旧文档到新文档

- `docs/requirements.md`
  - 迁移到父任务 `prd.md` 的范围说明
  - 详细需求基线迁移到 `06-27-requirements-baseline/prd.md`
- `docs/design.md`
  - 产品与架构方向迁移到父任务 `design.md`
  - 与修复直接相关的设计基线由 `06-27-fix-plan/prd.md` 引用
- `docs/implementation-plan.md`
  - 作为 `06-27-fix-plan/prd.md` 的阶段路线输入
- `docs/implementation-progress.md`
  - 仅提取“当前实现状态”和“历史偏差”信息
  - 不再作为当前事实来源
  - 所有“已完成/已接入/已可用”表述都必须在 Trellis 中重新分类为：
    - 已确认事实
    - 待程序现状验证
    - 已被用户反馈推翻的历史声明
- 当前代码、测试、fixture、CLI 行为
  - 迁入 `06-27-core-investigation/*` 的调查结论
  - 再由 `06-27-fix-plan/prd.md` 吸收为阶段排序依据

## Coverage Matrix

为了完成迁移，而不是只做摘要，旧文档的主要信息块按以下方式落位：

### `docs/requirements.md`

- 产品目标、价值排序、近期范围
  - 迁入 `06-27-requirements-baseline/prd.md`
- 智能解压、分卷、密码、编码、内嵌检测、输出路径、安全恢复
  - 迁入 `06-27-requirements-baseline/prd.md`
- CLI 命令、退出码、批量行为
  - 迁入 `06-27-requirements-baseline/prd.md`
- GUI、压缩、预览、系统集成、打包
  - 降级并记录在 `06-27-requirements-baseline/prd.md` 的 deferred / historical sections

### `docs/design.md`

- 渐进式修复方向、后端分工、事务式输出、动态工作流边界
  - 保留在父任务 `design.md`
  - 修复优先级和阶段化处理迁入 `06-27-fix-plan/prd.md`
- 密码策略、内嵌/分卷策略、安全预算、编码与后端边界
  - 当前交付所需的行为边界迁入 `06-27-requirements-baseline/prd.md`
  - 需要复现和验证的设计承诺迁入 `06-27-issue-inventory/prd.md`
- GUI/数据库/平台等中长期结构
  - 作为后续方向保留在旧设计输入和父任务 `design.md`，不再作为当前交付要求

### `docs/implementation-plan.md`

- 先修核心行为错误、再收敛可靠性与能力边界的阶段意图
  - 迁入 `06-27-fix-plan/prd.md`
- GUI、打包、长期结构演进
  - 明确降级为 deferred work

### `docs/implementation-progress.md`

- 历史实现声明、阶段结论、已知偏差
  - 迁入 `06-27-issue-inventory/prd.md` 的 historical claims / issue seeds
- 对当前优先级会造成干扰的“已完成”措辞
  - 迁入 `06-27-fix-plan/prd.md` 作为不能直接继承的历史完成依据

### 仓库流程说明

- `AGENTS.md`
  - 改为声明 Trellis 任务体系是主流程
- `docs/agents/issue-tracker.md`
  - 改为记录 `.trellis/tasks/` 的任务目录约定

## Task Tree Design

### 父任务职责

- 记录总目标、确认事实、迁移规则、子任务地图、统一验收标准
- 不承载逐项 bug 的根因明细，也不承载逐阶段修复步骤

### 子任务职责

- `requirements-baseline`
  - 先承接旧需求/设计文档里的有效产品目标、范围、验收口径
  - 再标出哪些旧需求已后置、缩小或不再作为当前交付
- `issue-inventory`
  - 先吸收旧计划文档和进度文档中的“已知问题/已完成声明/阶段假设”
  - 再用当前用户反馈和代码证据整理现象、证据、影响、优先级、验证
- `core-investigation`
  - 不再停留在“问题种子”，而是对 P0/P1 问题给出最小复现、代码路径、行为对照和初步根因
  - 作为 `fix-plan` 排序和后续修复拆分的直接输入
- `fix-plan`
  - 先承接旧 `implementation-plan` 中仍有效的阶段结构
  - 再结合 `core-investigation` 的现实证据，重排阶段顺序、依赖、验证方式、风险与非目标

## Design Constraints

- 不引入 `.scratch/` 与 `.trellis/tasks/` 双轨并存规则。
- 不在本轮全量填充 `.trellis/spec/`，避免把任务规划和长期编码规范混在一起。
- 文档内容必须反映现状，而不是继续沿用已经落后的 GUI-first 或全量平台交付叙述。

## Risks

- `docs/` 中仍可能保留与 Trellis 文档冲突的历史表述；本轮通过“明确 source of truth”降低风险，但不强制删除所有旧文件。
- 若后续修复工作直接引用旧 `docs/implementation-progress.md`，可能再次引入状态漂移；因此子任务文档中需要显式声明旧文档仅作历史参考。
- 如果先根据程序问题写新判断、却没有先把旧文档里的原始设计目标迁入 Trellis，后续会失去“设计意图 vs 实现偏差”的比较基线。
- 若迁移只覆盖高层标题、不迁移行为契约和历史状态分类，后续仍会回到“旧文档说完成了，但现在到底算不算完成”的混乱状态。
- 如果修复计划没有吸收实际调查结论，只按历史阶段和口头症状排序，后续实施仍会重复误判优先级。
