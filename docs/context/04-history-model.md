## History Model

| Term | Definition |
|------|------------|
| **TaskHistoryRecorder** | 调用方注入的历史记录接口，保存任务、事件和 per-file 解压动作，并更新 `known_files`。写库失败降级为 Warning，不中断解压。 |
| **DbTaskHistoryRecorder** | `TaskHistoryRecorder` 的 SQLite 实现，借用 `&rusqlite::Connection`。`Connection` 是 `!Sync`，因此 trait 不加 `Send + Sync` 约束——与已有的 `&PasswordService` 一样，解压 future 本就是 non-Send，仅 `.await` 不 spawn。 |
| **TaskOutcome** | 任务结束时交给 `finish()` 的聚合值。v3 精简为终态（completed/partial/failed/cancelled）+ 输出根路径；`encoding_selected` / `embedded_found` 等明细下沉到 `file_extractions`，不再作为 task 级聚合累积。 |
| **sample_hash** | 内容采样指纹：`BLAKE3(前 64KB ‖ 后 64KB)` 与 `file_size` 联合判等；文件小于 128KB 时全量哈希。内嵌归档按其范围采样，范围未知时不参与去重。 |
| **known_files** | `UNIQUE(sample_hash, size)` 复用索引，保存精确密码记忆、人工确认编码、成功解压时间和 name/offset 配对。自动猜测不覆盖人工确认编码。 |

### 完整性校验（2026-09-05）

- **VolumeSet**：只读收集同组卷、数值排序、入口、缺失/不可读与 identity/size/mtime 快照。RAR/字节切分从首卷开始，原生 split ZIP 从末段 `.zip` 开始。
- **TestArchiveReport**：一组一个报告，分开 integrity、coverage、password_status、confirmed_volumes、suspect_groups、missing/unreadable/unchecked 和带物理范围的依据。局部通过不代表整卷健康，候选组不能求交集得出确认坏卷。
- **Diagnostic pass**：engine 在失败主测试后发起的独立只读阶段，ArchiveExecutor 至多选择一个不同实现家族的后端；仍尊重强制 `--backend`，普通 corruption fallback 规则保持不变。本地格式校验不经过外部后端路由。
- DB **v4** 给 file_extractions 增加 nullable test_report_json，旧数据保留；damaged_volumes_json 只投影 confirmed 路径。test 不更新 known_files / last_extract_at，也不用首片 hash 表示整组。
- DB **v5** 仅重建密码排名索引，完整匹配含 COALESCE 的排序。导入和批量禁用使用单事务；导入保留重复行计数、pin 和重新启用规则，输入/SQL 错误回滚整批。
- 解压预算的全树/磁盘检查在阻塞工作线程运行，每个 monitor 仅一个检查在途；取消或后端结束先等待检查与后端回收，成功后做全新终检并返回累计 Usage。轮询是检查点预算，不是逐字节硬配额。
- 外部 test 非零退出可返回 `TestResult { ok: false, diagnostics }` 保留证据；调用者必须检查 ok。旧解压流程在既有 test-before-extract 分支把失败报告转换回错误状态，密码/损坏歧义不记密码失败统计。
