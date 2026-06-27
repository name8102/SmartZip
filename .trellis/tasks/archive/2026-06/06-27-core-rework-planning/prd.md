# SmartZip 核心功能梳理与修复规划

## Goal

将 SmartZip 当前分散在 `docs/` 中的规划材料迁移到 Trellis 任务体系，形成单一事实来源，并产出一套可继续执行的核心功能修复路线。

近期交付重点固定为：**核心解压能力可用 + CLI 可用**。GUI 保留为后续设计方向，不进入本轮修复交付。

本轮工作顺序固定为：

1. 先迁移旧 `docs/` 中仍有价值的需求、设计和阶段规划到 Trellis 父/子任务。
2. 再用当前程序真实问题去判断哪些旧文档需要修正、降级或标记为历史参考。
3. 文档基线收敛后，先进入代码层面的排查，再基于排查结果冻结修复顺序和后续实施拆分。

## Confirmed Facts

- 仓库已启用 Trellis，当前父任务目录为 `.trellis/tasks/06-27-core-rework-planning/`。
- 旧规划材料主要位于 `docs/requirements.md`、`docs/design.md`、`docs/implementation-plan.md`、`docs/implementation-progress.md`。
- 旧文档中既包含仍然有效的产品目标，也包含已经偏离当前现实的范围、阶段结论和“已完成”声明，不能整份直接照搬。
- 仓库当前存在流程冲突：`AGENTS.md` 与 `docs/agents/issue-tracker.md` 仍指向 `.scratch/`，但实际任务管理已在 `.trellis/tasks/` 下进行。
- 当前已有 3 个子任务用于拆分迁移结果：
  - `06-27-requirements-baseline`
  - `06-27-issue-inventory`
  - `06-27-fix-plan`
- 已知功能问题集中在解压成功率、分卷支持、密码流程、ZIP 编码和后端环境适配；递归限制测试属于历史报告，需复现后再决定是否纳入当前修复范围。

## Requirements

- 以 Trellis 作为项目规划文档的唯一事实来源；新需求、问题盘点、修复路线不再继续写入 `docs/` 下的旧规划文档。
- 父任务必须补齐 `prd.md`、`design.md`、`implement.md`，作为本轮迁移的总入口。
- 需求基线、问题清单、修复路线必须拆分到独立子任务，避免把长期文档挤在一个文件里。
- 迁移顺序必须先做“旧文档到 Trellis”的结构化搬运，再做“程序现状与旧文档的偏差校正”；不能跳过迁移直接写新的问题判断。
- 旧文档迁移时只保留仍与现状一致的内容；与现状冲突或已被产品方向否定的内容必须显式剔除或降级为历史参考。
- `docs/implementation-progress.md` 中的阶段完成描述只能作为历史输入，不能直接视为当前已实现事实；需要在子任务中重新落成“已确认事实 / 待验证 / 历史声明”三类。
- 修复规划必须围绕当前优先级组织：核心解压、CLI 可用、可靠性与测试回归优先；GUI、打包、系统集成后置。
- “问题排查”和“修复计划”必须保持为两个独立子交付：
  - `06-27-core-investigation` 负责代码、测试、样本和命令行为调查
  - `06-27-fix-plan` 负责把调查结果收敛成分阶段修复路线
- 修复计划不得只基于历史文档和口头现象排序；必须吸收调查任务产出的复现条件、代码路径、根因判断和验证入口。
- 每个子任务都要包含可验证的验收标准，而不是仅保留叙述性背景。
- 仓库中的 agent/workflow 说明需要与 Trellis 现状对齐，避免后续代理继续写入 `.scratch/`。

## Acceptance Criteria

- [ ] 父任务包含完整的 `prd.md`、`design.md`、`implement.md`，且三者职责清晰分离。
- [ ] `docs/requirements.md`、`docs/design.md`、`docs/implementation-plan.md`、`docs/implementation-progress.md` 的有效内容已映射到父/子任务文档。
- [ ] 父/子任务文档明确区分“旧文档迁移进来的内容”和“已被当前程序现状推翻或待验证的内容”。
- [ ] `06-27-requirements-baseline` 明确记录当前产品范围与需求基线，且强调 CLI 优先、GUI 延后。
- [ ] `06-27-issue-inventory` 形成按类别分组、带证据字段的问题清单模板。
- [ ] `06-27-core-investigation` 形成至少覆盖 `P0` 主流程问题的调查结论，可直接作为修复输入。
- [ ] `06-27-fix-plan` 形成按优先级和阶段划分的修复路线，并注明验证方式。
- [ ] `AGENTS.md` 与 `docs/agents/issue-tracker.md` 已改为指向 Trellis 工作流，而不是 `.scratch/`。

## Child Tasks

1. `06-27-requirements-baseline`
   - 迁移并收敛需求基线。
   - 明确当前交付边界和验收口径。
2. `06-27-issue-inventory`
   - 盘点已知问题、证据、影响范围和优先级。
   - 为后续修复子任务提供统一输入。
3. `06-27-fix-plan`
   - 将问题清单转成可执行的阶段性修复路线。
   - 规定验证命令、依赖关系和风险点。
4. `06-27-core-investigation`
   - 对高优先级问题做实际复现、定位和初步归因。
   - 产出可直接回灌到修复计划的证据和修复建议。

## Out of Scope

- 本轮不实现 GUI 工作台、密码库 UI、日志 UI 或系统集成 UI。
- 本轮不补写全部 `.trellis/spec/` package 规范，只记录相关缺口与后续任务入口。
- 本轮不处理打包发布与分发形态，例如 AppImage、dmg、bundled `7zz`。
