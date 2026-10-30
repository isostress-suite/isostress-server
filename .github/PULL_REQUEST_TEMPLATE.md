需要者：提交 PR 的人
不读会错什么：评审要问的问题答不全，PR 会被退回

# 合并请求

## 范围

本仓是**占位仓**。目前只有许可文件、双语 README 与安全策略——没有服务端实现。

请不要向本仓提实现类 PR。`isostress-docs` 里的协议规范描述了服务端角色，实现另行跟踪。

## 若你提交第一份实现

`isostress-client` 里的客户端实现是「线协议上什么叫正确」的参照。写服务端代码之前：

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

This repository is a **placeholder**. It currently contains licence files, a bilingual
README, and a security policy — no server implementation.

Please do not open implementation pull requests against it yet. The protocol
specification in `isostress-docs` describes a server-side role, and the implementation is
tracked separately.

## If you are adding the first implementation

The client implementation in `isostress-client` is the reference for what "correct" means
on the wire. Before writing server code:

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
