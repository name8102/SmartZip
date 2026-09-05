# Test 实施记录

> 2026-09-05 完成 T1–T6 的首版实现与验收。改动保留在当前工作区，尚未提交。

## 已完成切片

| 切片 | 实现与验证 |
| --- | --- |
| T1 · 报告与 VolumeSet | 结构化 integrity/coverage/password/evidence；RAR part/legacy、字节切分、native ZIP 分组，数值排序、任意入口、去重、缺失和快照。原生 ZIP 从最后 .zip 打开，不假定等长。 |
| T2 · 后端与 workflow | 7z/UnRAR/native ZIP test 保留失败报告；有界外部输出、终止子进程、密码尝试与分组汇总。旧 extract 的 test-before-extract 调用点转换失败报告以保持失败事件。 |
| T3 · RAR5 局部确认 | 检查 header CRC、可信范围截断、未加密非末段 packed CRC；坏解析链停止但继续其他卷。末段全文件校验不冒充本卷 CRC。 |
| T4 · 7z / ZIP 候选 | 7z start/next/encoded header、packed streams、全部 folder 输入、solid 依赖；ZIP disk/offset/local/descriptor/ZIP64 与 stored/deflate CRC。ZIP 校验元数据位置保留为候选；损坏组不求交集。 |
| T5 · 自动诊断与取消 | diagnose auto/off、追加阶段 timeout；最多一个不同实现家族的外部复核；尊重强制 backend，统一 TaskEvent/pass_id。缺卷后仍保留独立证据；输入变化使结论失效。 |
| T6 · CLI / JSON / history | 多组输出、JSON 不提示输入、0/1/2/130；DB v4 原地新增 nullable report，旧数据保留，历史可回读。只记真实验证的密码，不更新 known_files 或 last_extract_at。 |

## 验证结果

- `cargo test --workspace`：426 passed，0 failed，0 ignored，包含 unit、integration 和 doc tests。
- `cargo check --workspace --all-targets`、`cargo fmt --all -- --check`、routing guard 通过。
- `cargo clippy --workspace --all-targets` 成功，无新增代码 warning；现有 warning 与第三方 future-incompatibility 提示保留，详见验证记录。
- 真实 CLI 矩阵 18 组：7z solid / native split ZIP / RAR5 各覆盖完整、坏中卷、两卷同时坏、缺中卷、末卷截断、坏头。确认路径没有误指健康卷；候选覆盖变异范围或明确保留 unknown；全部源卷校验前后 SHA-256 一致。
- 额外 10 个 CLI 场景：加密 7z/ZIP 的无密码/正确/错误密码，混合批次、重复组、no-history、no-empty 未加密归档不误记密码。完整 JSON 与 DB 报告相同，known_files 未写入。
- 针对性回归含 ZIP64 split descriptor、RAR5 缺卷和独立损坏并存、7z 多输入 stream/早期短卷、重叠候选组、实际子进程取消、输入变化、只读 DB 写库失败、v3→v4 历史保留。

复现脚本、版本、种子与 hash 见 [产品验收记录](research/implementation-validation.md)。大型样本、原始报告和日志留在 `.work/test-implementation/`，不提交中间二进制。

## 验收中修正的问题

1. 新 TestResult 保留失败报告后，旧 extract 的 Ok(false) 分支曾吞掉错误密码失败事件；已在原调用边界转换为错误，原有集成测试通过。密码/损坏歧义不记失败统计。
2. 7z 未提供密码参数时，在 stdin EOF 返回 255，曾误报取消；test 现在明确传递空 `-p`，无密码的加密 7z 正确返回 required / exit 1。
3. 追加后端成功不能抹掉主后端损坏证据；ZIP CRC 参考元数据、7z 多 stream/solid 全依赖纳入候选，避免错误缩小到一个卷。
4. 外部成功日志可能含伪造文件名，不能证明密码被使用；无独立加密依据时不保存密码，原始及派生诊断中的密码文本均脱敏。

## 已知边界

- RAR4 无本地 checksum reader，日志本身不生成 confirmed；恢复卷/修复/PAR2 不在范围内。
- 加密 RAR5 分段校验变换、7z AES encoded header、任意 codec 的本地数据解码未实现。后端可以完整测试这些格式，本地不能定位时输出候选或 unknown。
- 7z 多输入映射回归使用 CRC 有效的合成 BCJ2 metadata，验证依赖映射，不声称完整 BCJ2 解码认证。未做所有 RAR4、solid/non-solid、加密多卷组合或跨平台终端认证。
- 后端主测试失败时保持 coverage=partial；即便存在 confirmed，也不声称其余卷完整。缺失或短卷导致原始字节偏移不确定时扩大候选。
- test 的终端密码输入沿用现有 prompter；隐藏输入/内容保真等 S1–S6 交互任务仍独立待做。`--use-clipboard` 仍明确为占位。
