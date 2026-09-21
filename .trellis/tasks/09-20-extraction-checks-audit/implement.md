# 常见解压场景检查成本审查

范围：用户要求查找不必要安全检查对性能和兼容性的影响；本次仅审查，未改实现。
依据当前工作树（包含此前 NFS 修复及其他未提交改动），不把已有改动作为本次结果。

## 已确认

1. budget::monitor_task 每 50ms tick，按耗时退避做全树检查，非全树 tick
   仍调用 statvfs。耗时退避上限为 1 秒，不能对非常慢的扫描保持固定占空比。
   inspect 对每个条目再次 symlink_metadata，初检与成功终检也走完整检查。
   多文件/高延迟文件系统存在重复元数据 I/O；可减少轮询频率或采用增量计数。
2. sevenzz 的目录预检一律拒绝符号链接/硬链接；budget::inspect 又拒绝非普通
   文件/目录和 nlink>1 的普通文件。普通安全相对链接包无法解压。建议按输出
   路径是否逃逸处理，并协调前检、产出检查和递归发现，不能只删一处检查。
3. min_free_bytes=0 仍执行 free_bytes，并传播 statvfs 错误；非 Unix 实现
   无条件返回 unsupported。应在阈值为 0 时跳过容量检查；容量不可查询的
   平台需合理退化。Windows 分支仅源码确认，未原生验收。
4. PreparedCommit::commit 无论有无状态记录器都创建恢复标记并 sync_all。
   --stateless 仍同步这个标记；可跳过不参与持久恢复的标记，普通有状态模式
   另行评估持久性要求。未实测 NFS fsync 延迟。
5. 默认累计输出 20GiB / 100000 entries / 512MiB 空闲下限，可能拦住大游戏、
   备份或源码依赖包；是产品默认值适配问题，不是已测得的吞吐瓶颈。

其他源码候选：decoded_zip::ensure_directories 对每个文件从 staging 根逐层
lstat 相同父目录；七压每个密码尝试都会运行完整列表预检且解析两次列表。
尚未为这些路径取得独立归因的性能基线，不给出加速数字。

## 有界复现

使用 cargo build -p smartzip-cli 后的 target/debug/smartzip，独立 /tmp 目录，
--no-config --stateless --no-recursive --non-interactive，结束清理所有样本。

- Python zipfile 创建 1500 个 64B 文件、6 层目录 ZIP，显式 GBK，min-free=0。
  strace -f -qq -e trace=%file,statfs,fsync：退出 0，1500 个文件产出。
  本次单样本全进程统计：newfstatat=10577, statx=2715, statfs=8, fsync=1。
  早期输出 0.txt 等路径各出现 4 次 stat；计数包含后端和扫描等工作，不可全部
  归因于 SmartZip 检查，更不是移除检查的加速比例。追踪会扰动轮询次数。
- Python tarfile 创建 data.txt='hello'，以及 shortcut.txt -> data.txt 的
  SYMTYPE 相对链接：退出 1，unsafe archive entry path: archive contains a link。
- 单文件 ZIP、min-free=0，strace -f -e trace=statfs
  -e inject=statfs:error=EIO:when=1：首次 staging statfs 被注入 EIO，CLI 退出 1，
  FAILED: I/O error at None: Input/output error (os error 5)。只给本次子进程注入。

建议先处理 1/2/3；保留绝对路径和 ../ 逃逸拒绝、旧输出提交回滚、密码保密。
没有修改源代码，没有吞吐 A/B 或 Windows/其他远程文件系统验收。


## 修复完成（2026-09-20）

用户授权以常见场景兼容性为先，冗余机制可直接移除。

- 输出文件数、字节上限默认改为 0（不限），磁盘余量默认 0（关闭）。
  已有 TOML 中显式设置的限制仍生效，不修改用户配置文件。
- 默认不扫描运行中的输出树；结束后做一次统计供历史/恢复使用。
  显式配置限额才启用至少 1 秒间隔的动态检查，慢扫描按耗时退避；
  只配置容量时不反复扫树。容量阈值 0 直接跳过查询。
- 外部后端不再因链接类型拒绝整个包；产出统计接受链接且不跟随符号链接。
  旧编码 ZIP 内部解压支持链接，延迟到普通文件写完后创建；父目录按路径
  缓存并 create_dir_all，移除每文件从根逐层 lstat。递归发现不跟随单根链接。
- 无状态提交跳过恢复标记创建和 fsync，覆盖回滚仍保留。有状态恢复不变。
- GUI 预览按 0=不限处理解压限额，仍保留其独立 8MiB 预览上限；CLI 帮助、
  GUI 设置说明和文档同步更新。目录成员预览规则保持原行为。

验证：archive/config/engine/CLI 的完整 cargo test 通过（明细见 validation.json），
覆盖显式资源限额、取消、累计统计、链接、提交恢复和真实后端集成。
GUI cargo check、cargo fmt 检查和 git diff --check 通过。

本地及 NFS 真实 TAR 验证普通相对符号链接、硬链接、悬空链接成功；启用默认
递归也成功。循环链接仍可被 7z 自身拒绝，本次不强行覆盖后端行为；引擎产出
统计接受循环链接但不跟随它。旧编码 ZIP 解压链接有直接单测，Windows 链接
权限与目录链接未做原生验收。

关闭容量查询时用 strace 注入 statfs EIO，解压成功。用户原始 ZIP 的 NFS
首次/覆盖解压使用独立数据库，每个文件大小及 CRC32 与原包一致，无暂存残留。

性能证据：同一 debug 构建环境，保留修复前二进制；交替运行本地 1500 个
64B 小文件（各 3 次）、NFS 500 文件（各 2 次），所有产出内容校验。
耗时原始样本见 validation.json；本地和 NFS 耗时有波动，不能证明稳定吞吐提升。
独立 strace 样本中 statx 2930→1580，statfs 8→0，fsync 1→0，
newfstatat 均为 10577。统计包含子进程且追踪会扰动轮询，不把调用减少比例
当作吞吐加速比例。测试数据、临时输出和旧二进制在验收后清理。

已构建 target/debug/smartzip；未替换全局安装程序。此前其他未提交改动保留。
