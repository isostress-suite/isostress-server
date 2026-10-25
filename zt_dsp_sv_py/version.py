# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (c) 2026 ZT-DSP Suite contributors

"""Version contract for the ZT-DSP server role.

The shape is the same as zt_dsp_py.version: one development baseline, not a
compatibility list.  The only difference is the identity of the role: a client
names itself with a language prefix (PY / RS), a server names itself with a
role prefix (SV), because one language may host several server roles.

The handshake frame carries no version string at all - neither the Phase 1
parameter JSON nor the Phase 4 AAD contains a version or protocol field.
"""

from __future__ import annotations

# --- Which document this implementation follows ---
# NOTE: this path points into the SIBLING repository ZT-DSP-docs.
# It does NOT resolve inside this repo -- this repo ships no copy of the
# protocol text.
FOLLOWS_DOC: str = "ZT-DSP Protocol Master Guide"
FOLLOWS_DOC_PATH: str = "ZT-DSP-docs/en/spec/ZT-DSP-1.0.2-Master_Guide.md"

# --- Document revision this implementation was written against (takes no part
# --- in network communication) ---
DOCS_VERSION: str = "1.0.2"

# --- Role implemented by this package ---
ROLE: str = "server"

# --- Role prefix.  Clients use a language prefix; servers use a role prefix. ---
LANGUAGE_PREFIX: str = "SV"

# --- Implementation version: the numeric part of a server version ---
CODE_VERSION: str = "1.0.0"

# --- The identifier of this implementation, e.g. ZT-DSP_SV-1.0.0. ---
IMPLEMENTATION_ID: str = f"ZT-DSP_{LANGUAGE_PREFIX}-{CODE_VERSION}"

# --- Client implementations this server is interoperable with. ---
CLIENT_VERSIONS: list[str] = ["ZT-DSP_PY-1.0.0", "ZT-DSP_RS-1.0.0"]

# --- Server implementations sharing the same development baseline. ---
SERVER_VERSIONS: list[str] = ["ZT-DSP_SV-1.0.0"]

# --- Name of the executable / entry point. ---
PROGRAM_NAME = "zt-dsp-server"


def format_version() -> str:
    """Single-line version string used by --version and the startup banner."""
    return f"{IMPLEMENTATION_ID} (docs {DOCS_VERSION})"


def asserts() -> None:
    """Self-check of the version contract.

    Raises AssertionError if the constants contradict each other.  Called at
    import time so that a broken contract fails loudly rather than producing
    an implementation that claims to speak a document version it does not.
    """
    assert ROLE == "server", ROLE
    assert LANGUAGE_PREFIX == "SV", LANGUAGE_PREFIX
    assert IMPLEMENTATION_ID == "ZT-DSP_SV-1.0.0", IMPLEMENTATION_ID
    assert FOLLOWS_DOC == "ZT-DSP Protocol Master Guide", FOLLOWS_DOC
    assert FOLLOWS_DOC_PATH.startswith("ZT-DSP-docs/"), FOLLOWS_DOC_PATH
    assert DOCS_VERSION == "1.0.2", DOCS_VERSION


asserts()
