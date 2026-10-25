#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (c) 2026 ZT-DSP Suite contributors
#
# Repository guard. Every check reports its own result. A check that cannot run
# is reported as FAIL, never silently skipped — the previous version piped
# through 2>/dev/null, so a broken git made the check pass.

set -u

fails=0
note() { printf '  ok   %s\n' "$*"; }
bad() { printf '  FAIL %s\n' "$*"; fails=$((fails + 1)); }

# 文件清单来源。失败必须向上返回非零，调用方要当错误处理。
if git rev-parse --git-dir >/dev/null 2>&1; then
  files() {
    local out
    if ! out=$(git ls-files -co --exclude-standard -- "$@"); then
      printf 'git ls-files failed for %s\n' "$*" >&2
      return 1
    fi
    printf '%s\n' "$out"
  }
else
  printf '  note not a git work tree, falling back to find\n'
  files() {
    local a dir pat
    for a in "$@"; do
      case "$a" in
        */*)
          dir=${a%%/*}
          pat=${a#*/}
          pat=${pat##*/}
          [ -d "$dir" ] && find "$dir" -type f -name "$pat" -not -path '*/target/*' -print
          ;;
        *)
          find . -type f -name "$a" -not -path './.git/*' -not -path './target/*' -print
          ;;
      esac
    done
  }
fi

# 载入文件清单。
#   0 = 有文件可查
#   1 = 列不出文件（已记 FAIL）
#   2 = 清单为空（记 note，不算失败）
load() {
  local label="$1"; shift
  if ! LIST=$(files "$@"); then
    bad "$label: cannot list files"
    LIST=""
    return 1
  fi
  if [ -z "$LIST" ]; then
    note "$label: no files matched"
    return 2
  fi
  return 0
}

# 在清单里搜模式：0=命中(bad) 1=未命中(ok) 2=出错(bad)
scan() {
  local label="$1" pattern="$2" want="$3"
  local -a arr
  mapfile -t arr <<< "$LIST"
  local hits rc
  hits=$(grep -nE "$pattern" -- "${arr[@]}" 2>&1)
  rc=$?
  case $want in
    none)
      if [ $rc -eq 0 ]; then
        bad "$label: pattern found"
        printf '%s\n' "$hits" | head -5
      elif [ $rc -eq 1 ]; then
        note "$label"
      else
        bad "$label: grep failed"
        printf '%s\n' "$hits" | head -3
      fi
      ;;
  esac
}

echo '== 1. licence headers =='
if load 'spdx' '*.rs' '*.sh' '*.py'; then
  hdr=0
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    if ! head -5 "$f" | grep -q 'SPDX-License-Identifier'; then
      bad "$f: missing SPDX-License-Identifier"
      hdr=$((hdr + 1))
    fi
  done <<< "$LIST"
  [ $hdr -eq 0 ] && note 'rs/sh/py carry SPDX headers'
fi

echo '== 2. no identity material in logs =='
if load 'identity-in-logs' 'src/*.rs'; then
  scan 'no log macro formats token hashes or session ids' \
       '(info|warn|error|debug|trace)!\([^)]*(token_hash|session_id|\.identity)' none
fi

echo '== 3. bilingual mirrors =='
mir=0
for f in README.md cn/README.md en/README.md; do
  if [ ! -s "$f" ]; then
    bad "$f missing or empty"
    mir=$((mir + 1))
  fi
done
[ $mir -eq 0 ] && note 'mirrors present and non-empty'

echo '== 4. no placeholder wording =='
if load 'placeholder' 'README.md' 'cn/**' 'en/**'; then
  scan 'no placeholder wording' \
       'placeholder repository|尚未开始实现|no implementation yet' none
fi

echo '== 5. no stale doc references =='
if load 'stale-doc-symbols' 'docs/*.md'; then
  scan 'no stale symbol references in docs' \
       'parse_phase4_header|TokenHash::hex_prefix|LOBBY_TIMEOUT|Phase4Header' none
fi

echo '== 6. version contract =='
vc=0
for sym in FOLLOWS_DOC FOLLOWS_DOC_PATH DOCS_VERSION IMPLEMENTATION_ID; do
  if ! grep -q "pub const $sym" src/version.rs; then
    bad "version contract missing: $sym"
    vc=$((vc + 1))
  fi
done
[ $vc -eq 0 ] && note 'version contract declared'

echo '== 7. shared test vectors =='
if [ -f test-vectors/vectors.json ]; then
  if python3 -c "import json,sys; json.load(open('test-vectors/vectors.json', encoding='utf-8'))" 2>/dev/null; then
    note 'test-vectors/vectors.json parses'
  else
    bad 'test-vectors/vectors.json is not valid JSON'
  fi
else
  bad 'test-vectors/vectors.json is missing'
fi

echo '== 8. third-party notices =='
if [ -f THIRD-PARTY-NOTICES.md ]; then
  if grep -q '^| crate | version | license |' THIRD-PARTY-NOTICES.md; then
    note 'THIRD-PARTY-NOTICES.md present'
  else
    bad 'THIRD-PARTY-NOTICES.md has no license table'
  fi
else
  bad 'THIRD-PARTY-NOTICES.md is missing'
fi

echo '== 9. version banner form =='
# The startup banner and --version both have to carry the document revision,
# and they have to agree. Running the built binary is the only way to see the
# shape a user actually gets; with nothing built yet, at least confirm the
# composed form still contains the suffix and that the banner reads it.
banner_bin=''
for c in target/release/zt-dsp-server target/debug/zt-dsp-server; do
  [ -x "$c" ] && banner_bin=$c && break
done
if [ -n "$banner_bin" ]; then
  v_out=$("$banner_bin" --version 2>/dev/null || true)
  case "$v_out" in
    *'(docs '*) note "built binary reports the document revision: $v_out" ;;
    *) bad "built binary omits the (docs <revision>) suffix: $v_out" ;;
  esac
else
  # Three assertions, each guarding one way this can regress:
  #   command().version(  -- clap is wired to the composed string, not the bare id
  #   format_version      -- the composed string still comes from version.rs
  #   '(docs '            -- the composed string still carries the revision
  # The base code passed a two-grep fallback with the clap wiring reverted,
  # so the wiring itself is asserted here.
  if grep -q 'command().version(' src/main.rs \
     && grep -q 'format_version' src/main.rs \
     && grep -q '(docs ' src/version.rs; then
    note 'version wiring carries the document revision (nothing built yet)'
  else
    bad 'version banner has no document revision'
  fi
fi
echo '== 10. python server tests =='
if ! command -v python3 >/dev/null 2>&1; then
  bad 'python3 not on PATH: the Python server tests cannot run'
elif ! python3 -c 'import pytest, zt_dsp_py' 2>/dev/null; then
  bad 'pytest or zt-dsp-py not importable: install zt-dsp-py (from the client repository) first'
else
  if python3 -m pytest zt_dsp_sv_py/tests -q >/dev/null 2>&1; then
    note 'zt_dsp_sv_py tests pass'
  else
    bad 'zt_dsp_sv_py tests failed'
  fi
fi
echo
if [ $fails -ne 0 ]; then
  printf 'check.sh: FAILED (%d)\n' "$fails"
  exit 1
fi
printf 'check.sh: ok\n'
