# Security Policy

## Supported versions

`ISOSTRESS_SV-1.0.0-20261030` is now an actual implementation. The data plane is the supported
surface.

| Version | Supported | Notes |
|---|---|---|
| `ISOSTRESS_SV-1.0.0-20261030` | Yes | current |

## Reporting a vulnerability

Use **private vulnerability reporting**:

1. Open the repository on GitHub.
2. Go to **Security** → **Advisories** → **Report a vulnerability**.

> That channel only works after **Settings → Code security → Enable private vulnerability
> reporting** is turned on. It is **off by default** on a new repository. Until it is enabled,
> open a public issue that contains no exploit details, and the maintainer will move the
> discussion to a private channel.

## What counts as in scope

- **In scope** — the implementation itself:
  - any path that lets the server observe plaintext it should not (phase-2/3/4 content),
  - any path that lets a probe distinguish a rejected handshake from a closed port,
  - pairing bugs that let a third party join a session it did not request,
  - memory exhaustion from forged length fields,
  - control-plane authentication bypass.
- **Also in scope** — protocol-document errors that would make implementations diverge,
  and `cn/` ↔ `en/` disagreements that touch a security statement.
- **Out of scope**: TCA/CRL behaviour (separate authority), traffic analysis by a global
  passive adversary, documentation wording, feature requests.

## Disclosures we would consider severe

| Class | Example |
|---|---|
| Confidentiality break | Server can reconstruct any session key or plaintext |
| Identity linkability | Server logs or derives who talks to whom |
| Pairing hijack | Attacker joins a session between two honest peers |
| Silent-drop defeat | A probe can enumerate valid token hashes via timing or reset behaviour |

## Disclosure timeline

Coordinated disclosure through the private channel. No fixed response-time commitment is
published; acknowledgement typically happens within a few days.
