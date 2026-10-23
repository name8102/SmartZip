# RAR fixture

`rar-ordinary.rar` derives from libarchive's
[`test_read_format_rar.rar.uu`](https://github.com/libarchive/libarchive/blob/master/libarchive/test/test_read_format_rar.rar.uu).
The symbolic-link member is removed; regular file and directory records are unchanged.
The payload of each file is `test text document\r\n`.

The associated [test source](https://github.com/libarchive/libarchive/blob/master/libarchive/test/test_read_format_rar.c) carries the following license:

```text
/*-
 * Copyright (c) 2003-2007 Tim Kientzle
 * Copyright (c) 2011 Andres Mejia
 * Copyright (c) 2011-2012 Michihiro NAKAJIMA
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE AUTHOR(S) ``AS IS'' AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES
 * OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
 * IN NO EVENT SHALL THE AUTHOR(S) BE LIABLE FOR ANY DIRECT, INDIRECT,
 * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT
 * NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
 * THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
 * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
 * THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */
```

SHA-256 (`rar-ordinary.rar`): `6514fbcc2ca1ca636ec20137dc864faffd13631dddb100befd92c3a9daeefa6d`.

`rar5-7zip-26.slt` is a captured `7zz l -ba -slt` listing from 7-Zip 26.02
for an encrypted RAR5 embedded after a JPEG prefix. Member names are anonymized;
empty field values intentionally retain the trailing space required by SLT.

## Single-stream fixtures

`single-stream.tar.{gz,bz2,xz,zst}` wrap the same 10,240-byte USTAR archive:
one regular member, `leaf_stream.txt`, containing `stream payload\n`, mode 0644,
mtime 0. Generated with Python's `tarfile` (`USTAR_FORMAT`), `gzip` (mtime 0),
`bz2`, and `lzma`; the zstd fixture uses `zstd --no-content-size` so decoding
must discover the output size. GZIP has no stored original filename.
These exercise nameless/unknown-size listings and recursive TAR extraction.
