---
name: rust-patterns
description: Rust ownership, error handling, module boundaries, and concurrency advice for implementation or review.
---

# Rust 建议

- 优先沿用项目已有模式。参数只需读取时借用；需要持有数据时再取得所有权，避免为绕过借用检查无意义地克隆。
- 用类型和枚举表达状态边界。抽象应服务真实复用或可测试性，公共接口尽量小。
- 对可预期的失败返回 `Result`，保留错误类别和定位上下文。对 `unwrap`、`expect` 或 `unsafe`，确认其不变量是否成立以及失败的影响。
- 先明确状态所有者，再选择共享、锁或通道。避免持锁跨越耗时操作或 `.await`；阻塞 I/O 按执行环境隔离，并考虑取消后的资源回收。
- 保留可解释的错误传播、清理和回滚路径。性能改动先定位成本，再以相同工作负载验证。
- 相关检查可用 `cargo fmt --all -- --check`、`cargo check -p <crate>` 和 `cargo clippy -p <crate>`；检查范围随改动影响调整。
