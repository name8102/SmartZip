# 闲置代码清理

## 范围与依据

- 在当前工作区的 GUI 重设计变更上工作，不回退或提交已有改动。
- 当前产品聚焦智能解压、浏览/预览、校验与任务恢复；压缩创建没有 CLI/GUI 生产调用。
- 删除压缩创建的请求/结果类型、压缩级别、executor/adapter 方法、后端实现、路由能力、专用断言和 mock 占位。保留现有解压、密码、取消、路径安全、历史迁移与恢复行为。
- 删除 archive 未使用的 `rstest` 开发依赖和 GUI 未使用的 `futures-util` 直接依赖；其他 crate 仍使用这些库，保留工作区依赖声明。
- 更新当前领域术语；旧设计文档中的压缩创建示例标记为历史方案。

## 验证

- `nix develop -c cargo test --offline -p smartzip-core -p smartzip-archive -p smartzip-engine`：464 通过，0 失败；2 项原有忽略项为子进程辅助入口与需 NFS 挂载的测试。
- `nix develop -c cargo check --locked -p smartzip-cli -p smartzip-gui --all-targets`：通过。
- 对修改的 Rust 文件执行 rustfmt；`git diff --check` 通过。
- 本次未执行原生 GUI 人工验收。
