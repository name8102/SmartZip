# SmartZip

SmartZip 是一个用 Rust 重写的跨平台压缩包辅助工具，目标是把**检测、递归解压、密码管理、编码处理**整合到统一工作流中。

> 说明：仓库已经从旧版 AHK 实现迁移到 Rust 工作区，旧脚本、旧截图和相关遗留资源已清理。

## 当前能力

- **检测**：识别嵌入式压缩包和伪装成普通文件的压缩数据
- **解压**：支持递归/嵌套解压，并可按需控制最大递归深度
- **密码管理**：支持密码列表查看、添加、删除、导入、导出与清理
- **编码处理**：支持自动识别与手动指定文件名编码
- **工作区**：CLI、GUI、核心库、扫描、密码、平台适配等模块分层组织

## 快速开始

查看帮助：

```bash
cargo run -p smartzip-cli -- --help
```

常用命令示例：

```bash
cargo run -p smartzip-cli -- detect <path>
cargo run -p smartzip-cli -- extract <path>
cargo run -p smartzip-cli -- password list
cargo run -p smartzip-cli -- password add <password>
```

`extract` 支持常见参数，例如：

- `--output <dir>`：指定输出目录
- `--deep`：启用深度扫描
- `--encoding <name>`：指定文件名编码
- `-p/--password <value>`：预置密码
- `--db <path>`：指定密码数据库文件

## 工作区结构

- `crates/smartzip-cli`：命令行入口
- `crates/smartzip-engine`：解压与扫描编排
- `crates/smartzip-archive`：压缩包后端抽象
- `crates/smartzip-scanner`：嵌入式压缩包/伪装数据扫描
- `crates/smartzip-passwords`：密码候选与排序逻辑
- `crates/smartzip-db`：密码数据库
- `crates/smartzip-config`：配置加载
- `crates/smartzip-core`：共享类型、错误与进度事件
- `crates/smartzip-platform`：平台相关能力
- `crates/smartzip-gui`：图形界面
- `docs/`：需求、设计、实现进展等文档
- `graphify-out/`：项目知识图谱输出（保留追踪）

## 开发

构建：

```bash
cargo build
```

测试：

```bash
cargo test
```

## 文档

- `docs/requirements.md`
- `docs/design.md`
- `docs/implementation-plan.md`
- `docs/implementation-progress.md`
- `docs/tech-evaluation.md`
- `CONTEXT.md`

## 许可证

MIT
