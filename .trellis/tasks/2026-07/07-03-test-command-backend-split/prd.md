# Test Command Backend (split from file-aware CLI)

## Goal

从 `07-02-file-aware-cli-commands` 中拆出 `smartzip test` 的真实后端实现。该任务专注于全量校验、损坏文件/坏卷定位、加密档密码处理，以及 `file_extractions` 上的 `intact/corrupt` 与 `damaged_volumes_json` 写入。

## Why Split

`test` 命令需要接入 7z/rar/zip 不同后端的全量校验路径，并区分密码错误、归档损坏、分卷缺失、可定位坏卷与不可定位损坏等状态，复杂度显著高于最初预估；继续与 detect/list 绑在同一任务中会阻塞 CLI MVP 落地。

## Scope

- 实现 `smartzip test <paths...>` 的真实后端。
- 复用共享求密码流程。
- 产出完整 stdout / JSON / history 语义：
  - `status=intact` / `status=corrupt`
  - `damaged_volumes_json`
  - damaged files 列表
  - damage localization（complete / incomplete / unknown）
- 维持与设计冻结规格一致的 exit code 语义。

## Dependencies

- `07-02-file-grain-history`
- `07-02-file-aware-cli-commands`（共享求密码流程、顶层 parser、history/task 框架）

## Acceptance

- `smartzip test` 不再返回未实现错误。
- 完好归档写 `intact`；损坏归档写 `corrupt`。
- 能可靠定位时写完整 `damaged_volumes_json`；不能定位时写空数组且在输出中反映 localization。
- 加密档能通过共享求密码流程测试；只有在可靠确认密码正确时才写 known_files.password_id。
- `test` 相关集成测试覆盖：完好、损坏、分卷缺失、密码正确/错误、不可区分损坏与密码错等路径。
