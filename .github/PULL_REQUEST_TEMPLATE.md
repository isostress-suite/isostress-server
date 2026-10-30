需要者：提交 PR 的人
不读会错什么：评审要问的问题答不全，PR 会被退回

# 合并请求

## 范围

本仓是服务端参考实现（Rust 数据面与 L4 控制面）与配套文档。协议规范在 `isostress-docs`。

实现类 PR 按 `CONTRIBUTING.md` 走：先开缺陷单，分支带编号，测试与守卫通过。

## 提交之前

`isostress-client` 里的客户端实现是「线协议上什么叫正确」的参照。改动服务端代码之前：

- [ ] 已读 Master Guide 里覆盖响应方角色的章节，尤其是 Phase 2 一节的时序约束。响应方必须先收完整帧、先算完 Diffie-Hellman，再回复，否则低阶点攻击者可以靠时序区分成功与失败。
- [ ] 静默丢弃行为与规范一致：参数不匹配时，响应方在线路上**什么都不发**。
- [ ] 已读本仓的 `SECURITY.md`。中英规范之间不一致，在本仓按**安全**问题处理，不按排版问题处理。

- [ ] 版本声明：版本数字是否前进；只换日期段时写出新日期

## CLA 与关联缺陷

CLA-Agreed: yes

外部贡献者填 yes 表示已读 LICENSING。CI 按整行精确读取本行，大小写与冒号不改。

Fixes #<缺陷编号>

没有关联就写 `Fixes #无`。
---

# Pull requests

## Scope

This repository holds the server reference implementation (Rust data plane and L4
control plane) and its documentation. The protocol specification lives in
`isostress-docs`.

Implementation pull requests follow `CONTRIBUTING.md`: open an issue first, name the
branch after it, and pass the tests and guards.

## Before you change server code

The client implementation in `isostress-client` is the reference for what "correct" means
on the wire. Before changing server code:

- [ ] The Master Guide sections covering the responder role have been read, especially
      the ordering constraints in the Phase 2 section. The responder must receive the
      full frame and complete the Diffie-Hellman computation **before** replying,
      otherwise a low-order-point attacker can distinguish success from failure by timing.
- [ ] Silent-drop behaviour matches the specification: on a parameter mismatch the
      responder must send **nothing at all** on the wire.
- [ ] You have read `SECURITY.md` in this repository. An inconsistency between the
      Chinese and English specification is treated as a **security** matter here, not as
      a formatting matter.

## CLA and linked issue

CLA-Agreed: yes

An outside contributor writes yes to confirm they have read LICENSING. CI reads this line verbatim.

Fixes #<issue number>

Write `Fixes #none` when the pull request has no linked issue.
