# 跨文件系统路径兼容实施结果

实施日期：2026-10-02。基于本目录原设计，生产链路已接通。当前采用 `BulkOriginal` / `ManagedAll`；设计中的 `HybridMapped` 不启用。需要映射时，由 SmartZip 创建输出对象，后端只提供按身份绑定的内容，不能先按超长原名解压再重命名。

## 实现与行为

- `smartzip-platform::path_policy` 在实际输出父目录探测文件系统、卷/根身份、名称长度及大小写/Unicode 比较语义。Native 使用实际卷规则，Portable 再叠加 Windows 名称和 255 UTF-8 字节限制；保守预算明确标注，排他创建是最终判断依据。
- `smartzip-archive::PreparedExtraction` 冻结完整成员清单、编码、adapter/可执行文件及全部输入卷身份。ZIP 直接读取物理中央目录原始名称，按索引流式读取；Unicode extra 与解码显示名不替换真实原始字节。外部后端在执行前后检查完整输入身份，准备阶段的候选回退保持原路由规则。
- `smartzip-engine::path_policy` 统一规划目录树及隐式父目录，按 grapheme 截取可读前缀，以带版本/域/长度 framing 的 BLAKE3 生成稳定后缀。碰撞组全部改名、预留未改名兄弟，并逐级延长摘要；不依赖枚举次序、entry ID 或暂存随机名。共享目录的映射作用于全部后代。
- 越界、绝对、驱动器/UNC、NUL、重复文件及文件/目录结构矛盾直接拒绝。兼容的重复目录声明可共用一个目录，所有原始声明仍留在报告；显式 `./` 根目录也保留。ZIP 库不能忠实按索引表示的物理重复成员明确拒绝。
- `ControlledRoot` 使用 POSIX 相对 FD 或 Windows verbatim/受保护父链创建文件，拒绝链接/reparse 和陌生对象。内容提取前先验证整个命名空间；只有确认的长度错误或已知实际别名可有界重规划，普通 I/O 失败不冒充名称问题。
- 受控 writer 在写入前预留跨 root 共享预算，验证完整大小、CRC/生产者终态及目录树。取消错误不使用会被 `write_all` 重试的 `Interrupted`，必须等待生产者退出及 writer 关闭后清理。
- 暂存目录保持 owner `rwx`，预算遍历和布局检查完成后恢复时间并捕获提交身份；Prepared 落库后才恢复最终目录权限。发布失败或 Prepared 重启清理先恢复本次暂存树访问权限，不修改旧用户备份或已发布输出。真实 `000` 根目录、`0400` 子目录已覆盖。
- 布局容器名、折叠名称和冲突后缀复用同一规则及总路径预算。若折叠会遗漏清单条目（含根声明或 `__MACOSX`），保留完整归档目录。提交前投影全部最终相对路径，冻结版本/摘要，复核输出父目录身份。
- SQLite schema v8 保存完整 `path_report_json` 和稳定 `path_reason`，独立于有界事件。CommitIntent 保存完整冻结报告、版本及摘要；成功恢复沿用原映射，校验失败保留原 intent 和未应用诊断证据，不重新规划或宣称已应用。
- CLI `--path-mode native|portable`、GUI 设置与排队任务共用配置快照。GUI 任务/历史详情按 50 项分页显示原名 → 最终名及原因；失败原因使用稳定中文标签，tentative 显示“计划调整”。CLI JSON 即使事件列表为空仍包含全部报告。

## 验证与可复现证据

本机：macOS / arm64 / 实际 APFS 卷，探测结果为大小写不敏感、规范化不敏感；Native 分量预算为保守 255 UTF-16 单元，完整 API 路径预算 1024。后端实际可用：7zz 26.02、p7zip 17.06、Unrar 7.30 beta 1；没有为本任务修改系统安装。

`cargo test --workspace --no-fail-fast`：**728 通过、0 失败、11 项既有 ignored**。其中包含 16 项规划测试、8 项平台路径测试、20 项 managed adapter 测试、8 项真实工作流兼容性测试及 69 项既有引擎集成测试。ignored 包含显式成本探针和既有文档示例，不作为本次性能证据。`cargo fmt --all --check`、`git diff --check` 和 CLI 构建通过。

真实归档覆盖：

| 场景 | 证据 |
| --- | --- |
| 中文/ZWJ emoji 长文件、长共享父目录、长扩展名 | 真实 ZIP Native/Portable 全流程及 7z 长名内容流；独立内容摘要比较 |
| Case/case、NFC/NFD、CON、冒号/问号、尾点/空格 | ZIP 全量原名、原始字节及最终映射；逆序成员得到同一内部路径 |
| 空目录、根别名、元数据成员、限制性目录权限 | 真实 ZIP 清单完整保留、最终布局及内容验收 |
| AES ZIP、Shift_JIS、Unicode extra、损坏/输入替换 | 原始身份、密码分类、CRC/完整生产者失败验证 |
| 7z solid、普通批量、取消、真实分卷 | 后端内容比对、所有卷身份冻结；原分卷/源回收回归不修改断言 |
| RAR4、TAR、GZIP | 真实内容读取；RAR fixture 来源/许可证/SHA 见 archive tests/fixtures |
| 预算失败、提交备份冲突、Prepared 重启与摘要篡改 | 旧输出保持、暂存清理/恢复、完整报告及原 intent 留存 |

生产 CLI 的独立 SHA-256 验收可重跑：

```sh
cargo build -p smartzip-cli
python3 scripts/path_policy_acceptance.py \
  --binary target/debug/smartzip \
  --output-report .trellis/tasks/09-22-filesystem-path-policy/research/cli-acceptance.json
```

[真实 ZIP](research/cli-acceptance.zip) 有 11 个成员，其中 10 个内容文件。脚本直接写归档头，不先把原长名称写到本机文件系统。Native、Portable 每个内容文件均与生成前的独立 SHA-256 一致；报告包含全部原名、原始名称字节和最终路径，并与 SQLite 和 CommitIntent 逐项一致。额外预算失败验证旧输出摘要不变、无 `.smartzip-*` 暂存、tentative 报告完整落库。[完整验收 JSON](research/cli-acceptance.json) 留存全部映射和摘要。

## 支持边界

| 范围 | 当前状态 |
| --- | --- |
| APFS Native/Portable | 本轮已在真实卷验证名称探测、受控提取、布局、预算、取消/清理和恢复 |
| Linux/Btrfs、Windows/NTFS | 平台代码与规则已实现，Linux/Windows platform 交叉编译通过；本轮没有对应物理卷原生验收 |
| 普通单卷 ZIP/7z，以及可完整列出并唯一选择的 7-Zip 普通成员 | 可受控映射提取；已完成上述实际格式验证 |
| 普通无需映射的 7-Zip 分卷 | 保留固定后端批量提取，全部卷身份绑定；已回归 ZIP/7z/spanned ZIP |
| 需要映射的分卷、Unrar 受控流、链接/特殊类型、选择歧义、无法完整恢复的特殊元数据 | 明确 `path_remap_unsupported`；不静默降级或丢弃成员 |
| 任意深路径 | 不承诺；名称和祖先可确定性缩短，无法在完整 API 预算内保留最小目录树时返回 `path_too_long` |

Hybrid、RAR5 映射、完整 xattr/ACL/ADS、三平台性能数据及 GUI 视觉截图验收尚未完成。solid 全受控路线可能重复解码；本次内容正确性测试不构成性能承诺。Windows 交叉检查启用 BLAKE3 pure feature，以避开本机缺失的 Windows C SDK；没有用交叉编译代替 NTFS 运行证据。
