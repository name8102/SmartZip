# Fix Plan Implementation Plan

## Ordered Steps

1. 固化问题列表
   - 按 P0/P1 分类现有问题
   - 明确每项的现象、影响和待验证点
2. 固化排查顺序
   - 先主流程，再落盘，再 CLI 契约，再能力边界
3. 固化修复阶段
   - 形成 Phase 0 到 Phase 5 的执行顺序
4. 标记本阶段边界
   - 明确当前不读代码
   - 明确当前不做根因判断和修复实现
5. 启动任务
   - 将 `06-27-fix-plan` 设为当前活动任务

## Validation

- `sed -n '1,260p' .trellis/tasks/06-27-fix-plan/prd.md`
- `sed -n '1,220p' .trellis/tasks/06-27-fix-plan/design.md`
- `sed -n '1,220p' .trellis/tasks/06-27-fix-plan/implement.md`
- `python3 ./.trellis/scripts/task.py start 06-27-fix-plan`

## Non-Goals For This Step

- 不读取业务代码
- 不运行功能测试
- 不修改实现文件

## Exit Condition

满足以下条件即可把该任务设为当前活动任务：

- `prd.md` 已包含问题列表、顺序和阶段计划
- `design.md` 已明确本阶段边界与排序原则
- `implement.md` 已明确当前动作和验证方式
