# test/t 产品验收记录

2026-09-05，在本地 Linux 工作区验证。与早期 24 次直接后端可行性实验分开计数。

## 可复现输入

- 基线：本地 `de0ea48` 加本次未提交实现；不以此表示远端最新状态。
- 工具：7-Zip 26.03、UnRAR 7.23、Info-ZIP 3.0；详见 [结构化结果](implementation-results.json)。
- 7z/ZIP 输入：Python Random seed=20260905，3 个 90 KiB 文件；7z LZMA2 solid，ZIP stored，分卷目标 64 KiB。
- RAR5：libarchive commit `ddf8247381814977c2f55a59f48d17460f7d00f0` 的 `test_read_format_rar5_multiarchive.part01..08.rar.uu` 数据。基线 SHA-256、每次变异 offset、截断长度与前后 hash 均在结果文件。
- Oracle 独立记录变异卷，生产诊断只读取归档。

```bash
cargo build -p smartzip-cli
python3 .trellis/tasks/2026-07/07-03-test-command-backend-split/research/verify_implementation.py
```

脚本从仓库根运行，所有生成物位于 `.work/test-implementation/`。首次需要联网读取固定版本的 RAR 数据；后续使用缓存。原始完整报告和 SQLite DB 留在 .work；版本化保存 compact JSON 与脚本。

## 分卷矩阵（18 个 CLI 调用）

| 格式 | 样本 | integrity | 确认卷 | 疑似组数 | 缺失卷 |
| --- | --- | --- | --- | --- | --- |
| 7z | good | intact | — | 0 | — |
| 7z | flip-middle | corrupt | — | 1 | — |
| 7z | flip-two | corrupt | — | 1 | — |
| 7z | missing-middle | incomplete | — | 0 | set.7z.002 |
| 7z | truncate-last | corrupt | — | 1 | — |
| 7z | header-flip | corrupt | set.7z.001 | 0 | — |
| zip | good | intact | — | 0 | — |
| zip | flip-middle | corrupt | — | 1 | — |
| zip | flip-two | corrupt | — | 2 | — |
| zip | missing-middle | incomplete | — | 0 | set.z02 |
| zip | truncate-last | unknown | — | 1 | — |
| zip | header-flip | unknown | — | 1 | — |
| rar | good | intact | — | 0 | — |
| rar | flip-middle | corrupt | set.part02.rar | 0 | — |
| rar | flip-two | corrupt | set.part02.rar, set.part07.rar | 0 | — |
| rar | missing-middle | incomplete | — | 0 | set.part02.rar |
| rar | truncate-last | corrupt | set.part08.rar | 0 | — |
| rar | header-flip | corrupt | set.part02.rar | 1 | — |

全部完好基线完整通过；每个变异样本未被判 intact。confirmed 是真实修改集合的子集，RAR5 单/双中卷变异准确确认实际修改卷；所有明确缺中卷均保留 missing。候选组覆盖变异范围，无法建立范围时 localization=unknown。全部现存源卷在 SmartZip 调用前后 SHA-256 相同。

## 密码、历史与退出码（10 个 CLI 调用）

| 场景 | exit | password_status | 报告组 / 历史行 |
| --- | --- | --- | --- |
| 7z-missing | 1 | required | 1 / 1 |
| 7z-correct | 0 | verified | 1 / 1 |
| 7z-wrong | 1 | indeterminate | 1 / 1 |
| zip-missing | 1 | required | 1 / 1 |
| zip-correct | 0 | verified | 1 / 1 |
| zip-wrong | 1 | rejected | 1 / 1 |
| mixed | 2 | not_needed, not_needed | 2 / 2 |
| deduplicated | 0 | not_needed | 1 / 1 |
| no-history | 0 | not_needed | 1 / 0 |
| no-empty-clear | 0 | indeterminate | 1 / 1 |

逐个比较 JSON 报告与 test_report_json 完全相同，旧 damaged_volumes_json 恰好投影 confirmed 路径；known_files 没有写入。正确凭据仅在完整验证后入库，错误或未使用的密码无命中记录。

## 项目检查

- `cargo test --workspace`：426 passed / 0 failed / 0 ignored；含现有解压与真实后端集成测试。
- `cargo check --workspace --all-targets`、`cargo fmt --all -- --check`、`bash scripts/check_routing_guards.sh` 通过。
- `cargo clippy --workspace --all-targets` 成功。现有 warning：PlatformPaths 的 Default、router 的 vec 初始化、engine 旧文档空行/参数数量/默认字段赋值/循环、旧集成测试的借用和 map_or、GUI format、CLI 旧函数参数数量。新增 test/diagnostic 文件无 warning。第三方 proc-macro-error2 有既存 future-incompatibility 提示。
- 针对性检查：ZIP64 跨卷 descriptor、7z 多输入与不可信短卷偏移、RAR5 独立多故障与缺卷、实际 Linux 子进程取消、输入变化、DB 只读写入失败，以及 v3→v4 历史保留。

## 结论边界

这些结果验证首版功能及所列样本，不是所有压缩算法或跨平台认证。RAR4 无本地解析；加密分卷/7z AES header、任意 codec 可能无法定位。7z BCJ2 测试只覆盖多输入 metadata 的依赖映射，实际解码由外部后端负责。失败后的部分检查不能证明整卷健康，报告保留 partial/unknown。
