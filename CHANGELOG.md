# Changelog

All notable changes to this repository are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

**Note on authority:** this file is a convenience record, not the source of truth. The
governing change history is `git log`. If the two ever disagree, `git log` wins.

## [Unreleased]

### Added

- **Server implementation (Python).** `zt_dsp_sv_py/` — the server role in Python:
  frame relay, lobby pairing, silent drop, profile tiers — imported from
  `ZT-DSP-client` @ `b413078`, where it had landed by mistake: the server role
  belongs in this repository. It depends on `zt-dsp-py`, installed from the client
  repository by git URL because the package is not on PyPI.
- **CI for the Python implementation.** A `python` job (ubuntu-latest, 3.10–3.14)
  runs the pytest suite and asserts the `--version` contract; a `package` job
  builds the wheel/sdist and smoke-tests an install. The `zt-dsp-py` dependency is
  pinned to a client commit and moves only by hand.
- **A tenth check** in `scripts/check.sh`: the Python server test suite runs there
  too, so the repository guard covers both implementations.
- **Server implementation (Rust).** Data plane implementing ZT-DSP 1.0.1:
  - Appendix F.1 phase-1 frame parsing with F.6 length limits enforced before allocation.
  - Appendix F.2 strict silent drop: no bytes written, no active close, 8192-byte drain,
    diagnostics on stderr only, indistinguishable from a closed port.
  - §4.2 lobby pairing with mutual-match predicate and a 30 s timeout (Appendix H.3).
  - Bidirectional byte relay that never re-encodes or inspects phase 2–4.
  - Appendix B.2 profile tiers (`beginner`, `advanced`, `high-anonymity`).
- **L4 control plane** (documented extension, separate port, JSON Lines):
  - offline envelope queue with FIFO eviction and TTL,
  - group log with per-member cursors,
  - identity directory with minute-coarsened timestamps and arbitrary lookup off by default.
- `docs/ARCHITECTURE.md`, `docs/PROTOCOL-CONFORMANCE.md`, `docs/CONTROL-API.md`,
  `docs/THREAT-MODEL.md`, `docs/DEPLOYMENT.md`.
- End-to-end and L3/L4 test scenarios under `tests/`.

### Changed

- **The version contract declares one development baseline.** `APPLIES_TO_DOC_VERSIONS`
  is gone from `src/version.rs` and from the imported `zt_dsp_sv_py/version.py`
  (which also drops its `DOCS_VERSIONS` / `DOCUMENT_REVISION` shape); the followed
  document is stated by `FOLLOWS_DOC` / `FOLLOWS_DOC_PATH` / `DOCS_VERSION`, and the
  client/server pairing tables (`CLIENT_VERSIONS` / `SERVER_VERSIONS`) stay. Per the
  2026-10-25 ruling: no multi-guide compatibility is declared before release.
  `check.sh` item 6 now pins `DOCS_VERSION` instead of the removed list.
- **`--version` and the startup banner come from one source.** clap receives the
  same composed string the banner prints, so the two exits cannot disagree. The
  output carries the document revision, e.g. `ZT-DSP_SV-1.0.0 (docs 1.0.2)`.
  The clap `string` feature owns the value; no allocation is leaked for it.
- **The conformance reference is the 1.0.2 guide.** `version.rs` declares
  docs 1.0.2 and points at the published 1.0.2 file. `PROTOCOL-CONFORMANCE.md`
  cites clauses (§1.3, §5.1) instead of line numbers, which drifted between
  document revisions; the least-privilege row previously named §4.2 while its
  cited line sits in §1.3 in both 1.0.1 and 1.0.2.
- **The implementation is split into modules.** `src/main.rs` held every module
inline as a convenience; each protocol surface now has its own file: `frame.rs`
(Appendix F), `lobby.rs` (pairing), `silent.rs` (silent drop), `relay.rs` (byte
relay), `config.rs` (B.2 tiers), `l4.rs` (offline, groups, directory),
`control.rs` (JSON Lines), `version.rs`, `metrics.rs`, `util.rs`, with `main.rs`
holding only the process wiring. Module boundaries and visibility are
unchanged, which the compiler and the test count confirm: the same 59 unit and
3 integration tests pass with no edit to a single test. `check.sh` now reads
the version contract from `src/version.rs`.

- **The dead pairing field is gone.** Outcome::Matched carried the peer's
  Phase 1 frame, but production destructured it as `peer_frame: _` and threw
  it away; only a test read it. The field and the entry slot that fed it are
  both removed, and the test now asserts something stronger: that the peer
  which arrived first receives the frame of the peer which arrived second,
  read off its own handoff channel.
- **Configuration loading has its own error type.** Both failures used to be
  flattened into io::ErrorKind::InvalidData, so a caller could not tell
  "cannot read the file" from "the file is not valid JSON", and the
  original error was only reachable as text. ConfigError now carries the
  path and the source error for each case, and implements Error::source.
- **The scattered timeouts are configuration now.** The control-plane read and
  write timeouts, the byte-relay write timeout, the accept backoff and the two
  sweep cadences were literals spread across the file. They are six fields on
  Config with documented defaults, and the validator refuses a zero.

- **Relay return type no longer lies**: relay_pair never produced an Err, so its
  io::Result wrapper was dead ceremony. It now returns the tuple directly.
- **L4Error is a real error type**: implements std::error::Error and PartialEq,
  and carries a stable machine-readable code(). The control plane now answers with
  error_code next to the human-readable error text, so machine clients no longer
  have to parse prose.
- **Mutex poisoning no longer panics a connection**: all 17 expect sites go through
  a lock() helper that recovers the inner value. Every locked section is panic-free,
  so poisoning can only come from a cancelled task, and the counters and maps are
  still self-consistent.
- **write_frame has a write timeout**, so the phase-1 replay during a pairing
  handoff cannot hang a connection forever.
- **Configuration is validated at startup** instead of being silently clamped at
  runtime: the nine .max(1) clamps are gone, and a zero value now stops the process
  from listening. Values belonging to a disabled feature are not checked.
- **The accept loop backs off** on error instead of retrying at full speed.
- **ct_eq compares length in constant time too** - the length difference no longer
  short-circuits the loop.
- **read_line_limited fills the buffer once**; the previous two-call pattern could
  desynchronise the chunk length if the buffer was rewritten in between.
- **A failed response serialisation is no longer swallowed** into a fake ok:false
  body.
- **The control plane rejects unknown fields.**
- **The profile serialised name has a single source of truth** (serde kebab-case);
  the duplicate as_str is gone and a test pins the wire names.
- **A redundant guard arm** in the frame reader was removed, and a stale
  allow(dead_code) on a field that is read was dropped.
- **THIRD-PARTY-NOTICES.md** lists all 57 dependencies with the licence taken from
  each pinned version's own Cargo.toml.
- **test-vectors/** reuses the client repository's shared vectors, with a test that
  keeps the -v3 derivation-label convention in lockstep with the client.

- **Source consolidated into a single file** (`src/main.rs`), per the collection rule in
  `ZT-DSP-client/cn/README.md`: one implementation is one file, and the mainline directory
  does not grow a directory tree. Module boundaries are kept as inline `mod` blocks, and
  their `pub` surface is narrowed accordingly.
- **Version contract declared** in `src/main.rs` (`mod version`): `FOLLOWS_DOC`,
  `FOLLOWS_DOC_PATH`, `APPLIES_TO_DOC_VERSIONS`, plus `IMPLEMENTATION_ID` and
  `format_version()`. The same shape as `zt_dsp_py/version.py` and `zt_dsp_rs/version.rs`,
  with a role prefix (`SV`) instead of a language prefix. `--version` and the startup
  banner read from it.
- **CI added** (`.github/workflows/ci.yml`): a matrix of native `ubuntu-24.04` and
  `ubuntu-24.04-arm` runners, each building its own target, running fmt, clippy, tests,
  a release build, and asserting the produced binary's `readelf` machine matches. This is
  what makes the "both targets build" claim checkable; actions are pinned to commit SHAs
  and no secrets are referenced.
- `scripts/check.sh` gained two checks: `src/` must contain exactly one `.rs` file, and
  the version contract symbols must be present.
- `README.md`, `cn/README.md`, `en/README.md` rewritten from placeholder text to describe
  the implemented server, with a conformance table pointing at the specification.
- `scripts/check.sh` rewritten: it previously rejected any `.rs`/`.toml`/`.py` file and
  demanded placeholder wording in the README, which is no longer true now that an
  implementation exists. It now checks licence headers and mirror consistency instead.
- `SECURITY.md` updated: there is now a deployed surface to report against.

### Fixed

- **A word-sweep commit mangled the legal texts.** The 2026-10-18 sweep mapped
  "official" to "published" in the English trademark section and 「官方」 to
  「参考」 in the Chinese one. The prohibition list then read "published,
  reference, canonical" / 「参考」「参考」「标准」 — publishing a fork is
  allowed, so the list forbade something permitted and lost its object — and
  the package names stopped being marked out as the official ones. The same
  commit put the nonexistent word "publishedly" into CODE_OF_CONDUCT.md. All
  four TRADEMARK lines and the CoC word are restored to their pre-sweep
  wording, byte-identical to the text before the sweep and matching the
  client and docs trademark files.

### Removed

- The "placeholder repository" notice from all three READMEs.

### Notes

- TCA token issuance and CRL distribution remain out of scope: they are a separate
  authority, not part of the server role.

## [1.0.0]

- Initial placeholder: licence files and bilingual README.
