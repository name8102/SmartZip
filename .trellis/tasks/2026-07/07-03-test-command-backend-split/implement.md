# Implementation Plan — Test Command Backend

在 `07-02-file-aware-cli-commands` 提供的共享求密码流程、顶层 parser 与 history/task 框架之上，补齐 `smartzip test` 的真实后端。重点是把“密码正确性”和“归档完整性”分离建模，并尽量可靠地定位 damaged files / damaged volumes。

## 阶段一：后端能力梳理与抽象

- 调研当前 7z / zip / rar 后端各自的全量校验路径。
- 明确可稳定拿到的信号：
  - 完整通过
  - 明确损坏
  - 明确密码错误
  - 无法区分密码错 / 归档损坏
  - 可定位坏卷 / 不可定位坏卷
  - 可枚举 damaged files / 只能得到粗粒度错误
- 在 engine 层抽象统一的 `TestArchiveOutcome` / `DamageLocalization` / `DamagedFile` / `DamagedVolume` 结果模型，供 CLI、JSON、history 复用。

## 阶段二：engine test 流程

- 实现 `test_archive(...)`：
  - 复用共享求密码流程。
  - 正确区分：
    - 密码正确且归档完好 → `intact`
    - 密码正确但归档损坏 → `corrupt`
    - 需要密码但无法获取/用户取消 → `skipped + password_required`
    - 无法判断密码错还是归档损坏 → 不写成功密码，输出保守诊断
- 只有在“可靠确认密码正确”时才写/更新 `known_files.password_id`。
- 只有在后端能可靠定位时才填 `damaged_volumes_json`；否则写空数组。

## 阶段三：CLI / JSON / history 接线

- `smartzip test <paths...>` 多输入执行。
- stdout 输出符合冻结规格：状态、坏卷列表、坏文件列表、localization。
- JSON report 接上 `files[]` 结果与 task summary。
- history 写 `file_extractions.status=intact/corrupt` 与 `damaged_volumes_json`。
- exit code 符合冻结优先级：`3` 用于至少一个 corrupt，`4/5/2/0` 服从整体汇总语义。

## 验证

- 单元 / 集成测试覆盖：
  - 完好 ZIP/7z/RAR
  - 损坏归档
  - 分卷缺失/坏卷可定位
  - 密码正确
  - 密码错误
  - 用户取消密码输入
  - 无法区分密码错误与损坏
- 全 workspace `cargo test` 通过。
