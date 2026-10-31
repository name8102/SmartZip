# 内嵌归档格式覆盖修复

## 触发样本与根因

用户提供的 MP4 在偏移 `43324603` 处拼接了 AES-256 ZIP，ZIP 长度为
`1159919967` 字节，随后还有 `14353` 字节附加数据。ZIP 的三个文件头和
中央目录能够对应。binwalk 3.1.0 的方法白名单遗漏 AES 标记 `99`，
SmartZip 的文件扫描和字节扫描均在目录验证前被该规则过滤。

相邻规则检查还发现 ZIP 的方法 `16`、`20`、`93..=97` 和 alternate-stream
标志位被遗漏；LZMA 的签名只覆盖部分字典和 properties，头解析还拒绝
小于 256 字节或超过 4 GiB 的已声明解压大小。

## 实现决定

- 在 SmartZip 中覆盖 ZIP/LZMA 的 binwalk parser，并让文件扫描复用同一
  头校验规则，覆盖 Fast/Deep、有限/无限扫描和窗口边界。
- ZIP 按已定义方法识别容器；AES 额外验证加密标志、扩展字段、版本、
  vendor、密钥强度和内部方法。是否能解码仍由后端路由决定。
- 保留中央目录关联和边界验证。只有头、缺少目录时保持未知长度，
  不把扫描窗口末端当成归档末端。
- LZMA 补齐 liblzma 支持的 properties 和 4 KiB 起的便携字典编码，
  用有界内存的流解码确认结果，不根据输出大小的经验范围过滤。
  文件和字节扫描使用 64 MiB 解码内存限制；损坏/不完整的流不输出 finding。
- 实际 MP4 元数据能被解释为无 EOS、无输出的 18 字节 LZMA 流；该弱证据
  降为 Low，不出现在默认结果中。带 EOS 的合法空流仍正常识别。
- 不修改 binwalk 依赖、不引入新依赖，不修改用户原文件。

格式依据：[PKWARE APPNOTE 4.4.4/4.4.5](https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT)、
[WinZip AES](https://www.winzip.com/en/support/aes-encryption/)、
[XZ 的 LZMA 文件格式说明](https://github.com/tukaani-project/xz/blob/master/doc/lzma-file-format.txt)。

## 验证

- 修复前新增回归测试复现 AES ZIP、现代 ZIP 方法、小字典 LZMA 和小定长
  LZMA 的四类漏检；另用真实 MP4 元数据复现扩大识别范围后的空流误报。
- `nix develop --command cargo test -p smartzip-scanner`：27 项测试通过，覆盖扫描入口、
  非法 AES 字段、未知目录边界、LZMA 参数/大小/损坏和弱证据过滤。
- 真实 7-Zip 后端集成用例
  `test_extract_aes_zip_from_video_carrier_with_trailing_data` 通过：
  从视频前缀及尾部附加数据中切出 AES ZIP，使用测试密码解压，验证内容
  和原文件未改变。
- 实际用户文件使用本次 CLI 构建执行 stateless detect；只做识别和后端
  probe，结果为 `status=detected`、`encrypted=true`、`needs_password=true`，
  默认结果仅有上述偏移和长度对应的一个高置信度 ZIP。未对用户资源执行
  解压或程序运行。
- `cargo fmt --all -- --check` 通过。
