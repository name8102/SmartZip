# 共享后端源归档回收与分卷支持

## 实现

- 将 GUI 的解压后回收逻辑移入 `smartzip-engine::source_cleanup`，通过 `SmartZipEngine::with_source_recycling` 开启；默认关闭，GUI 沿用 `delete_source`，不新增 CLI 参数。
- 使用引擎实际成功提交的归档和获胜分卷组记录，回收整个组的原始文件。支持只选首卷、中间卷或全部卷；同组重复输入产生的去重跳过不再阻止回收。未采用的分组候选和无关同目录文件保留。
- 整批成功后调用共享的 `ArchiveRecycleHandler`，默认移入系统回收站。失败、取消、源文件变化、空输出等情况下保留原包；回收错误通过引擎 Warning 事件和 GUI 结果报告。
- 复用提交时已计算的输出文件数，去掉 GUI 对输出树的重复遍历。源文件仅检查元数据，不读取整包内容；回收通过阻塞线程执行。
- 自动恢复旧任务不启用源回收。嵌入归档载体继续保留，不从成功解压的子归档推断可删除载体。

## 验证

- `cargo test -p smartzip-engine -p smartzip-cli -p smartzip-gui`：394 passed，7 ignored，0 failed。
- `cargo test -p smartzip-gui --test runtime_worker source -- --include-ignored`：5 passed。包含真实后端普通归档回收、取消和源变化保留，以及分卷首卷输入、数据库任务全部卷输入。
- 新增引擎真实后端集成测试：普通 ZIP/7z、`.zip.001`、`.7z.001`、原生 `.z01 + .zip`；单文件/首卷/中间卷/全部卷共 12 种输入组合，各验证失败保留和成功完整回收、内容一致、无关文件保留。另验证失败分组候选不会被回收。
- `cargo build -p smartzip-cli -p smartzip-gui`、`cargo fmt --all -- --check`、`git diff --check` 通过。仅构建工作区二进制，未替换系统安装版本。

## 验证边界

- RAR 共用分卷成员记录与回收路径，但环境缺少 `rar` 创建工具，未生成真实 RAR 分卷验收。
- 原生分卷 ZIP 使用默认 Auto 编码通过。测试中额外发现显式 UTF-8 Override 会进入既有 decoded ZIP 路径并报 invalid local header，此问题不属于本次回收改动，尚未修复。
- GUI 验证覆盖 runtime worker，未进行桌面点击验收；回收测试仅使用新建临时归档。
