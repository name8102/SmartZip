# P0-1 回归测试执行计划

## Ordered Steps

1. 查阅 `crates/smartzip-engine/tests/smartzip_integration.rs` 的现有 fixture/helper 风格。
2. 构造小型 `.tar.gz` fixture：gzip 内含 tar，tar 内含 `leaf.txt`。
3. 构造小型 `zip -> .tar.gz -> .tar -> leaf.txt` fixture。
4. 构造小型 `zip -> inner.zip` 单文件中间归档 fixture，确保覆盖单文件输出后继续 nested 解压的路径。
5. 为每个 fixture 写目标行为断言：最终 `leaf.txt` 或内层内容成功落盘，不出现 archive-file-as-directory 路径。
6. 跑定向测试，记录当前失败输出到任务 notes 或 research 文件。

## Validation

```bash
cargo test -p smartzip-engine --test smartzip_integration nested
```

如果测试名不包含 `nested`，改跑新增测试的精确名称。

## Commit Note

这些测试预计先红后绿。不要把长期红测试单独留在主线；如需分开提交，必须有明确的 TDD 流程说明或临时 ignore 策略。
