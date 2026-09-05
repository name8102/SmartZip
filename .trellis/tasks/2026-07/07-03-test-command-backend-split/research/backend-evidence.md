# 分卷诊断实验

2026-09-05；仅在临时样本拷贝上制造错误，随后删除样本。没有接线 SmartZip test。

## 方法

- 7-Zip 26.03：7z t -bb1 -bd；UnRAR 7.23：unrar t -p- -y，stdin 关闭。
- 7z/ZIP：种子 20260905，3 个各 90 KiB 随机文件，64 KiB 分卷，共 5 卷。7z 使用 LZMA2、solid；ZIP 使用 zip 的原生 split。
- RAR5：libarchive 官方 8 卷样本，固定提交 ddf8247381814977c2f55a59f48d17460f7d00f0；只下载和解码数据，不执行源码。[样本来源](https://github.com/libarchive/libarchive/blob/ddf8247381814977c2f55a59f48d17460f7d00f0/libarchive/test/test_read_format_rar5.c)
- 每组状态：完好、中卷翻转、两卷翻转、缺中卷、末卷截断 80 字节、头部 offset 12 翻转。内容翻转在 7z/ZIP 卷内 offset 6000，RAR offset 500。
- 共 24 次后端 test：7z/ZIP 各 6 次，RAR 6 种状态×2 个后端。完好基线全通过，变异均非零退出。
- 逐次输出、RAR 原始 SHA-256 及工具版本见 [机器记录](probe-results.json)。

## 关键结果

| 变异 | 观察 | 推论 |
| --- | --- | --- |
| RAR 第 2 卷内容翻转 | UnRAR 点名 part02 的 packed-data checksum 错误；7z 只报告内部文件 CRC 失败 | 有具体卷定位机会，后端诊断价值不同 |
| RAR 第 2、6 卷同时翻转 | UnRAR 分别点名两卷，7z 报两个坏文件 | 不应只返回首个错误 |
| RAR 缺 part02 | missing volume 之外还有文件校验错误 | 防止级联错误误报其他坏卷 |
| RAR 末卷截断/第二卷头部错误 | 一般截断/头损坏文本未稳定附物理卷名 | 仍需卷内结构检查 |
| 7z 中卷内容翻转 | 仅内部文件 CRC Failed | 需要压缩依赖与物理范围映射 |
| 7z 缺中卷或末卷截断 | 错误中出现首卷 .001 和 Unexpected end | 首卷路径不是坏卷证据 |
| ZIP 缺 z02 | 明确 missing，并伴随内部文件错误 | 缺失独立展示 |
| ZIP 内容翻转 | 仅内部文件 CRC Failed | 进一步定位需要目录与数据范围 |

只验证了这些版本和样本。RAR4、加密分段、7z 多 packed stream、ZIP64、取消和输入变化仍待产品级验证。

## 一手格式依据

- [RARLab](https://www.rarlab.com/technote.htm)：头部/分段字段、末段与加密校验语义不同。
- [7-Zip](https://raw.githubusercontent.com/ip7z/7zip/main/DOC/7zFormat.txt)：stream、folder 与可选 digest 可用于范围关联，不能假设每个物理卷都有独立 CRC。
- [PKWARE](https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT)：磁盘号、局部偏移、split 命名与不等长段。
