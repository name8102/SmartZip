# Linux 原生接口原型

本目录是研究材料，不属于 Cargo workspace，不进入安装产物。

## 固定输入与环境

- 7-Zip 26.06：`d3296e39e1f1f92c7b41c0c3848e65640428d42d`。
- Linux x86-64，Rust 1.98.1，GCC 16.2.0，Python 3.14.8。
- 测试库为官方 `Format7zF` 完整构建；本次本地二进制 SHA-256：`ae4331b443798f66ce784dce6b3b8abfc2c60161fe53a5d1dfda24332e6a7455`。这是实验产物校验值，不是 Nix source hash。
- 7z 加密样本由本机 `7zz 26.04` 生成，ZIP/TAR/GZip/XZ 由 Python 生成；**被测后端始终是 26.06 原生库**。生成器不是产品后端。

## 复现

从 SmartZip 仓库根目录执行，下例临时目录可自行替换：

```bash
git clone --depth 1 --branch 26.06 https://github.com/ip7z/7zip.git /tmp/7zip-research
git -C /tmp/7zip-research rev-parse HEAD

make -C /tmp/7zip-research/CPP/7zip/Bundles/Format7zF \
  -f makefile.gcc -j"$(getconf _NPROCESSORS_ONLN)" \
  CFLAGS_WARN=-Wno-error=array-bounds

R=.trellis/tasks/11-10-sevenzip-library/research
LIB=/tmp/7zip-research/CPP/7zip/Bundles/Format7zF/_o/7z.so

# 首选方案：编译过程没有 C++ 编译/链接步骤。
rustc --edition=2021 -O "$R/native_probe.rs" -o /tmp/smartzip-native-rust-probe
python3 "$R/run_probe.py" /tmp/smartzip-native-rust-probe "$LIB" --plain-only

# ABI/密码行为对照：官方 C++ 头文件，不是拟增加的产品桥接层。
c++ -std=c++17 -O2 -I/tmp/7zip-research "$R/native_probe.cpp" \
  /tmp/7zip-research/CPP/Common/MyWindows.cpp -ldl -o /tmp/smartzip-native-probe
python3 "$R/run_probe.py" /tmp/smartzip-native-probe "$LIB"
```

默认上游 `makefile.gcc` 含 `-Werror`，本机首轮在 `HandlerCont.cpp` 的 array-bounds 告警失败；上面的参数仅用于本次研究，将该类告警降级。未判定告警成因，未修改上游源码，也未全局关闭警告。完整构建命令的日志位于本机 `/tmp/smartzip-7zip-native-build.log`，汇总及原型原始输出保存在 [results.json](results.json)。

## 结果

| 检查 | Rust 直接 FFI | C++ 对照 |
| --- | --- | --- |
| ZIP / 7z / TAR / GZip / XZ list+test | 5 项通过 | 5 项通过 |
| 按 index=0 读取 23552 字节，不落盘 | 通过 | 通过 |
| CRC 损坏：S_OK + kCRCError(3) | 通过 | 通过 |
| 回调取消：E_ABORT，写出 0 字节 | 通过 | 通过 |
| 数据密码正确/错误 | 未实现该原型回调 | 2 项通过；错误返回 S_OK + kDataError(2) |
| 加密头缺密码/正确/错误 | 未实现该原型回调 | 3 项通过；缺密码 E_ABORT，错误 S_FALSE，均观察到密码回调 |
| 合计 | **8/8** | **13/13** |

ZIP 样本含中文、换行、伪 `Path = ...` 字段和两个同名成员，API 按独立索引返回。Python 的 duplicate-name warning 来自有意构造的样本。

两个原型都直接调用 `CreateObject`、`IInArchive` 和自定义流。C++ 原型先用于头文件对照；Rust 原型随后验证无需 C++ 桥接即可调用同一库。`run_probe.py` 解析的是研究程序自己的断言输出，不是 7-Zip CLI 文本协议。

## 原型边界

- Rust 原型手写 x86-64 Linux 最小 ABI，C++ 原型密码仅 ASCII。两者只验证必要接口，不是可复用的生产实现。
- 动态库在进程生命周期内保持加载；生产实现需完整模块/COM/variant RAII、panic 边界、原始 I/O 分类及 ABI layout 验证。
- 原型按 handler 顺序尝试、从 offset 0 打开，不包含产品所需的 SFX/格式候选策略。
- 原型读取/丢弃载荷，不创建输出文件。未验证分卷、RAR、旧编码、非 BMP 名、worker、超时硬终止、Windows/macOS 或性能；完整验收要求见 [design.md](../design.md#11-实施顺序与验收门槛)。
