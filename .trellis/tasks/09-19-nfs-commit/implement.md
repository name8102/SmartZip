# NFS 输出提交兼容

用户报告本地解压成功，输出到 `/mnt/storage/Downloads` 时出现 EINVAL。
真实挂载探针确认：NFS 4.2 上 renameat2(RENAME_NOREPLACE) 返回 EINVAL，
普通 rename 成功；本地支持原子防覆盖。

用户明确选择自动兼容，接受并发防覆盖保证降低，并要求以常见解压场景的
性能和兼容性为先。实现及竞态边界见 docs/cli-beta.md 的输出章节。

额外发现此挂载重命名目录会改变 inode，导致覆盖提交误报 output changed
 during commit。身份比较在 inode 不符、设备/类型/大小/mtime 匹配时才检查
文件系统；仅 Linux NFS 允许这一兼容匹配。正常 inode 匹配无新增文件系统查询。
共享身份比较也用于恢复；状态记录格式保持不变。

验证：
- engine 229 项测试通过，含 4 项新增回退/错误/冲突/回滚测试和 1 项真实 NFS
  文件、目录重命名身份测试。NFS 用例默认 ignored，本次通过
  SMARTZIP_TEST_NFS_DIR=/mnt/storage/Downloads 和 --include-ignored 执行。
- 真实 CLI 后端在 NFS 独立临时目录对用户的 80,502,537 字节 ZIP 首次解压、
  覆盖解压成功；每次 10 个文件大小和 CRC32 均与 ZIP 一致。
- 真实 CLI 单文件 flat-single 输出成功。使用独立本地数据库、显式 GBK、
  --no-config、--no-recursive；未做网络中断/崩溃恢复验收或性能基准。
- 验收结束时无暂存/备份残留，所有测试目录已清理；源文件未修改。
  覆盖过程中 CLI 曾报告 backup retained，但结束时该备份已由后续清理移除。
- cargo fmt --all -- --check 和 git diff --check 通过。
- cargo clippy -p smartzip-engine --lib -- -D warnings 被既有 smartzip-platform、
  smartzip-db、smartzip-config 告警阻断，未修改无关模块。
- 已构建 target/debug/smartzip；未替换 ~/.cargo/bin/smartzip。
