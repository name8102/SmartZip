# Core Investigation Execution Plan

## Ordered Steps

1. 建立排查输入
   - 收集现有问题描述
   - 识别可用样本、fixture、命令路径和测试入口
2. 第一轮排查主流程
   - 分卷
   - 密码
   - 批量/嵌套
3. 第二轮排查落盘与后处理
   - 目标路径
   - 覆盖
   - 删除
4. 第三轮排查 CLI 契约
   - 多输入交互
   - 错误提示
   - 退出码
5. 第四轮排查能力边界
   - ZIP 编码
   - 原生 ZIP 后端
   - fallback / 环境依赖
6. 汇总结论
   - 逐项写出当前行为、根因判断、修复建议、验证建议

## Validation

- `sed -n '1,260p' .trellis/tasks/06-27-core-investigation/prd.md`
- `sed -n '1,220p' .trellis/tasks/06-27-core-investigation/design.md`
- `sed -n '1,220p' .trellis/tasks/06-27-core-investigation/implement.md`
- `python3 ./.trellis/scripts/task.py start 06-27-core-investigation`

## Exit Condition

可启动条件：

- `prd.md` 已明确排查目标、范围、顺序和验收
- `design.md` 已明确调查方法和证据来源
- `implement.md` 已明确执行顺序
