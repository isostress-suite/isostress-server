# Shared test vectors

`vectors.json` **不是本仓原创**：它的字节由 `isostress-client` 仓的
`test-vectors/generate_vectors.py` 生成，三仓各存一份**逐字节相同**的副本。
规范附录 J 规定以 `isostress-docs` 仓那份为权威副本；本卡之后三份 sha256 相同，
所以「逐字节取自 client」与「以 docs 为准」不再是两句话，而是同一份字节。

服务端复用它，而不是各写一份 —— 一致性向量一旦分叉就没有意义。

## 这里有什么

| 文件 | 内容 |
|---|---|
| `vectors.json` | 共享一致性向量，10 条，`note` 版本 1.0.0 |

- 当前指纹：`sha256 = da526e0e81f159e6bf3fdc6819a8d4a62f6524cadc3c058838a5218c7e7eaf55`
- 三仓同一指纹：`isostress-client` / `isostress-server` / `isostress-docs` 的 `test-vectors/vectors.json`
- 行尾：LF（`*.gitattributes` 已锁 `* text=auto eol=lf`）

## 按什么找

- 消费点：`src/version.rs` 的 `labels_match_the_spec_verbatim`。
- 生成器：`isostress-client/test-vectors/generate_vectors.py`（`--out PATH` 可指定输出位置）。
- 复现性门禁：用 `isostress-client/test-vectors/generate_vectors.py` 重生成到临时路径，
  与已提交文件逐字节比对；不一致即判失败。

## 三份怎么保持一致

1. 在 `isostress-client` 里跑 `python test-vectors/generate_vectors.py`。
2. 把生成的 `vectors.json` **按字节**复制到 `isostress-server/test-vectors/` 与
   `isostress-docs/test-vectors/`（`cp` / `copy`，不要用编辑器另存）。
3. 三份 `sha256` 必须相同；不同即视为分叉，先查生成器版本再查行尾。

## 服务端怎么用它

服务端**不解密、不派生密钥**，所以这 10 条向量里 9 条密码学向量对它没有直接约束。
但派生标签本身是服务端必须与规范保持一致的间接契约：
客户端与服务端对标签串的理解一旦分叉，双方就永远通不了。

`src/version.rs` 里的 `labels_match_the_spec_verbatim` 就是守这条的。
它把 7 个标签的期望值逐字写死再比对，**不是**套「都该带 `-v3` 后缀」
这种规则：规范里 `INFO_EK_ROTATE` 对应的串是 `ISOSTRESS-EK-ROTATE`，
本就不带后缀，套规则会把它误判成漏了。

## 第 10 条向量是什么

`openssl-sha3-256-hkdf-crosscheck` 于 2026-10-28 并入三份共享集：
它原先只存在于 `isostress-docs` 那份，导致三份条数分叉（9 / 9 / 10）。
该向量由 OpenSSL 3.5.9 的 `kdf` 命令产出，覆盖协议自己的 `info` 标签
（`ISOSTRESS-EK-v3`），是第三方工具交叉校验；规范附录 J.2 本来就按 10 条描述，
并入后规范、docs、client、server 四处口径一致。
