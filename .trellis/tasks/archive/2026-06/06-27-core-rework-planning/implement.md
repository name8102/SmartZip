# SmartZip 文档迁移执行计划

## Ordered Steps

1. 迁移旧文档到 Trellis
   - 从 `docs/requirements.md` 迁移需求目标、范围、验收口径
   - 从 `docs/design.md` 迁移架构方向、后端策略、边界与非目标
   - 从 `docs/implementation-plan.md` 迁移阶段结构与历史优先级
   - 从 `docs/implementation-progress.md` 迁移历史完成声明与偏差信息
2. 补齐父任务文档
   - 完成 `prd.md`、`design.md`、`implement.md`
   - 固定 Trellis 为唯一事实来源
3. 创建并填充子任务
   - `06-27-requirements-baseline`
   - `06-27-issue-inventory`
   - `06-27-fix-plan`
   - `06-27-core-investigation`
4. 基于程序现状做偏差校正
   - 标出哪些旧文档内容仍成立
   - 标出哪些内容已过时、需修正或仅保留为历史参考
   - 把真实程序问题落到 issue inventory 和 fix plan
5. 执行调查并回灌
   - 在 `06-27-core-investigation` 中按优先级复现和定位问题
   - 把调查出的复现条件、代码路径、根因判断和验证入口回灌到 `06-27-fix-plan`
6. 对齐仓库流程说明
   - 更新 `AGENTS.md`
   - 更新 `docs/agents/issue-tracker.md`
7. 验证迁移结果
   - 检查父任务 `children` 是否正确
   - 检查每个子任务 PRD 是否具备目标、要求、验收标准
   - 检查子任务已明确迁移来源与历史状态分类
   - 检查 `core-investigation` 与 `fix-plan` 的输入/输出关系已写清楚
   - 检查仓库说明不再指向 `.scratch/`

## Validation

- `python3 ./.trellis/scripts/task.py list`
- `sed -n '1,220p' .trellis/tasks/06-27-core-rework-planning/prd.md`
- `sed -n '1,220p' .trellis/tasks/06-27-core-rework-planning/design.md`
- `sed -n '1,220p' .trellis/tasks/06-27-core-rework-planning/implement.md`
- `sed -n '1,260p' .trellis/tasks/06-27-requirements-baseline/prd.md`
- `sed -n '1,260p' .trellis/tasks/06-27-issue-inventory/prd.md`
- `sed -n '1,260p' .trellis/tasks/06-27-fix-plan/prd.md`
- `sed -n '1,260p' .trellis/tasks/06-27-core-investigation/prd.md`
- `sed -n '1,220p' AGENTS.md`
- `sed -n '1,220p' docs/agents/issue-tracker.md`

## Rollback / Safety

- 本轮仅修改文档与任务元数据，不触及业务代码。
- 如果后续决定恢复双轨文档体系，应新建独立任务处理，而不是回退本轮 Trellis 主线迁移。

## Follow-up

- 若准备进入实际修复开发，先完成 `06-27-core-investigation` 的证据收集，再把结论固化到 `06-27-fix-plan`，然后按阶段拆修复子任务。
- 另起任务梳理 `.trellis/spec/` 缺口，不与当前修复规划任务混做。
