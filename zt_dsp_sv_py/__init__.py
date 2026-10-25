# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (c) 2026 ZT-DSP Suite contributors

"""ZT-DSP server role.

The implementation lives in one module, zt_dsp_sv_py.server.  The version
contract lives in zt_dsp_sv_py.version and is deliberately separate, because
the contract is a declaration about the outside world while the server is the
implementation of it.
"""

from .version import (
    CODE_VERSION,
    DOCS_VERSION,
    FOLLOWS_DOC,
    FOLLOWS_DOC_PATH,
    IMPLEMENTATION_ID,
    PROGRAM_NAME,
    ROLE,
    format_version,
)

__all__ = [
    "CODE_VERSION",
    "DOCS_VERSION",
    "FOLLOWS_DOC",
    "FOLLOWS_DOC_PATH",
    "IMPLEMENTATION_ID",
    "PROGRAM_NAME",
    "ROLE",
    "format_version",
]
