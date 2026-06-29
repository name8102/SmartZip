# P0-1 补嵌套路径冲突回归测试

## Goal

在修改实现前，补齐嵌套归档输出路径冲突的最小回归测试，明确当前失败和修复后目标行为。

## Problem

中间归档被 materialize 为单文件路径后，后续 nested candidate 会把该文件路径当作目录 prefix 使用，导致 `File exists (os error 17)`。

## Requirements

- 只增加测试和小 fixture/helper，不修实现。
- 测试样本必须小，避免超大文件和磁盘压力测试。
- 覆盖 `.tar.gz -> .tar -> leaf.txt`。
- 覆盖 `zip -> .tar.gz -> .tar -> leaf.txt`。
- 覆盖 `zip -> inner.zip` 且 inner 被布局为单文件输出的路径。
- 不复用只能覆盖纯 zip 多层成功路径的 `nested_multi_level.zip` 作为唯一证据。
- 保留现有纯 `zip -> zip -> zip` 成功路径测试。

## Suggested Files

- `crates/smartzip-engine/tests/smartzip_integration.rs`
- fixture/helper 可以放在现有 engine integration test 附近。

## Acceptance Criteria

- [ ] 新增 `.tar.gz -> .tar -> leaf.txt` 目标行为测试。
- [ ] 新增 `zip -> .tar.gz -> .tar -> leaf.txt` 目标行为测试。
- [ ] 新增单文件 inner archive 输出路径目标行为测试。
- [ ] 记录当前实现下失败证据，包含 `File exists` 或同类路径冲突现象。
- [ ] 未修改 engine/archive/CLI 实现逻辑。
- [ ] 现有 `nested_multi_level.zip` 成功路径仍作为回归范围的一部分。

## Out Of Scope

- 不修 `output_dir_for_candidate` 等路径模型。
- 不处理资源限制、truncated ZIP、encoding、CLI JSON 等其他问题族。
