# 单流解压覆盖与数据库一次性迁移

## 请求与原因

- 修复用户样本 `.tar.zst` 的 `path_remap_unsupported`；完成一次性数据库迁移，移除旧库兼容，并更新本机安装。
- p7zip 17.05 和 7-Zip 26.04 对该 zstd 流均仅列出空 `Size`/`Packed Size`，没有 `Path`；严格成员清单解析因此失败。
- 解开外层后，真实 TAR 的单行 PAX 注释和普通文件的零值 Device Major/Minor 又被误拒绝。

## 实现

- GZIP/BZIP2/XZ/Zstandard 走强制格式的单流解码。预检在已有字节预算、取消和进程回收机制下计数、计算 CRC，不写输出；冻结清单后再按精确大小/CRC 受控写入。无成员名时从输入名推导，保留可准确表示的 GZIP 名称/时间，TAR 简写后缀展开。
- 单流不调用成员选择器，也不使用整包原名提取。普通归档仍验证清单、成员总数及唯一选择。TAR 接受单行 PAX 注释和零设备号，继续拒绝注入、链接及特殊文件。
- CLI/GUI 默认只用当前数据库路径；删除旧目录选择、`schema_migrations` 回退及其历史测试。v9 删除仅用于兼容的 `known_files.last_extract_at`，完成事实仍在 `file_extractions`。
- 独立 `scripts/migrate_database.py` 用 SQLite backup 复制已提交 WAL，0600 临时文件、完整性检查及原子不覆盖发布；应用运行时不保留旧路径迁移/回退。

## 验证与本机交付（2026-10-24）

- archive managed 测试 27 项通过；涵盖四种流、新旧 7-Zip、预算、损坏、伪装容器、PAX 和已有注入/取消回归。
- engine 四种 TAR 压缩包装的真实递归测试 4 项通过（默认测试栈）；预检解码 future 装箱，避免扩大调用方栈。
- smartzip-db 54 项、history_integration 5 项通过；一次性迁移 Python 测试 5 项通过。
- CLI/GUI 检查、release 构建与安装成功；格式及 diff 检查通过。
- platform 13 项通过，已有 Automator 测试遇到系统“无法与帮助程序通信”，未扩展修复。
- 真实用户样本使用临时归档和数据库副本，默认递归完成 3 层，退出 0、跳过 0；TAR 内 149 个普通文件共 200,952,574 字节，逐文件 SHA-256 与输出一致；原归档保留。
- 本机旧库已迁到 `~/Library/Application Support/SmartZip/smartzip.db`，原路径保留作备份。安装后首次默认打开升级至 v9；7 张表保留列的全部数据与备份逐行一致，quick_check 通过。
- CLI 安装到 `~/.cargo/bin/smartzip`，哈希与 release 一致；GUI 安装到 `~/Applications/SmartZip.app`，与签名包一致且 codesign 严格校验通过。
