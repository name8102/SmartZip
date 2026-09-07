---
name: rust-testing
description: Choose meaningful Rust tests and validation scope for changed behavior, regressions, and performance claims.
---

# Rust 测试建议

- 测试可观察的行为与失败边界，避免照抄实现或给每个函数机械补测试。修复缺陷时优先保留能复现问题的回归用例。
- 纯逻辑用单元测试；跨模块、文件系统、外部后端和数据库行为用适当的集成测试。先参考邻近用例和现有依赖。
- 临时目录和独立数据库有助于隔离副作用；异步测试使用可控同步和有界等待，减少依赖固定延时。
- mock 适合隔离边界，但不能证明真实归档后端可用。缺失外部工具或样本时明确说明验证缺口。
- 先运行受影响 crate 或测试：`cargo test -p <crate> [filter]`。共享契约或广泛改动可扩大到 `cargo test --workspace`。
- TDD、属性测试和覆盖率工具按任务需要选择，不设统一覆盖率门槛。性能结论需要可比基线和测量，普通测试通过不足以证明加速。
