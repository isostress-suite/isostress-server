// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite contributors

//! 仓库守卫：日志宏不得格式化身份材料。
//!
//! 规范禁止服务器记录阶段一的配对信息。令牌哈希与会话标识都属于身份材料，
//! 出现在日志里就等于把配对关系写了下来。
//!
//! 这道检查此前只在 `scripts/check.sh` 里，而 CI 从不调用那个脚本——它只对
//! 手工跑过它的人生效。`#17` 进一步把脚本撤出跟踪，那时这道检查在本仓就
//! 完全不存在了。既然 CI 本来就跑 `cargo test`，检查放在这里才真的守得住：
//! 脚本在不在仓库里，这道检查都在。
//!
//! 检查必须看得见**跨行书写**的事件宏。仓库自己就有跨行的 `debug!`
//! （`src/main.rs` 的版本契约那段），单行正则会漏掉它，于是这道检查形同虚设。
//! 下面按字符配对括号取整个宏调用体，换行与嵌套括号都绕不过去。

use std::path::{Path, PathBuf};

/// 会被记进日志的事件宏。
const EVENT_MACROS: [&str; 5] = ["info", "warn", "error", "debug", "trace"];

/// 属于身份材料的记号。
const IDENTITY_MARKERS: [&str; 3] = ["token_hash", "session_id", ".identity"];

/// 源文件目录。
fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// 从 `open_idx`（左括号之后）取到与之配对的右括号为止的整个调用体。
///
/// 跳过字符串字面量：字面量里的括号不该影响配对，反斜杠转义也不能被当成
/// 引号结束。
fn call_body(src: &str, open_idx: usize) -> &str {
    let bytes = src.as_bytes();
    let mut depth = 1usize;
    let mut i = open_idx;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                // 跳过整个字面量：开引号之后一直走到闭引号为止。
                // 闭引号本身留给循环末尾统一前进一格——多走一格会把字面量
                // 后面的右括号也吃掉，配对就再也合不上了。
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    // 反斜杠转义：跳过被转义的那个字节。
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open_idx..i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    &src[open_idx..]
}

/// 找出文件里所有格式化身份材料的事件宏，返回 (行号, 宏名)。
fn identity_leaks(src: &str) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    for macro_name in EVENT_MACROS {
        let needle = format!("{macro_name}!");
        let mut from = 0usize;
        while let Some(rel) = src[from..].find(&needle) {
            let start = from + rel;
            from = start + needle.len();

            // 宏名必须是完整的一个标识符：前面不能是标识符字符。
            if start > 0 {
                let prev = src.as_bytes()[start - 1];
                if prev.is_ascii_alphanumeric() || prev == b'_' {
                    continue;
                }
            }
            // 宏名后面只能跟空白和左括号。
            let rest = &src[from..];
            let trimmed = rest.trim_start();
            let skipped = rest.len() - trimmed.len();
            if !trimmed.starts_with('(') {
                continue;
            }
            let body = call_body(src, from + skipped + 1);
            if IDENTITY_MARKERS.iter().any(|m| body.contains(m)) {
                let line = src[..start].matches('\n').count() + 1;
                hits.push((line, macro_name.to_string()));
            }
        }
    }
    hits.sort();
    hits
}

#[test]
fn no_log_macro_formats_identity_material() {
    let dir = src_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("读得到 src/ 目录")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();

    assert!(!files.is_empty(), "src/ 下应当有 .rs 文件");

    let mut leaks = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file).expect("源文件是 UTF-8");
        for (line, macro_name) in identity_leaks(&src) {
            leaks.push(format!(
                "{}:{line}: {macro_name}!() formats identity material",
                file.strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
                    .unwrap_or(file)
                    .display()
            ));
        }
    }

    assert!(
        leaks.is_empty(),
        "日志宏不得格式化令牌哈希或会话标识：\n{}",
        leaks.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_line_macro_is_caught() {
        let src = "    debug!(token_hash = %h, \"leak\");\n";
        assert_eq!(identity_leaks(src).len(), 1);
    }

    #[test]
    fn a_multi_line_macro_is_caught() {
        let src = "    debug!(\n        token_hash = %h,\n        session_id = %s,\n        \"leak\"\n    );\n";
        let hits = identity_leaks(src);
        assert_eq!(hits.len(), 1, "跨行书写必须同样被抓住：{hits:?}");
        assert_eq!(hits[0].0, 1);
    }

    #[test]
    fn a_clean_macro_passes() {
        let src = "    debug!(peer = %peer, frame_len = n, \"phase1 accepted\");\n";
        assert!(identity_leaks(src).is_empty());
    }

    #[test]
    fn a_nested_call_does_not_end_the_body_early() {
        // body 里有括号：配对必须数到最外层那个右括号。
        let src = "    info!(x = some_fn(a, b), \"ok\");\n";
        assert!(identity_leaks(src).is_empty());

        let src = "    info!(x = some_fn(a, b), token_hash = %h, \"leak\");\n";
        assert_eq!(identity_leaks(src).len(), 1);
    }

    #[test]
    fn a_bracket_inside_a_string_does_not_confuse_the_scan() {
        let src = "    info!(note = \"a ) b\", \"ok\");\n";
        assert!(identity_leaks(src).is_empty());

        let src = "    info!(note = \"a ) b\", token_hash = %h, \"leak\");\n";
        assert_eq!(identity_leaks(src).len(), 1);
    }

    #[test]
    fn a_macro_name_that_ends_with_an_event_macro_is_not_matched() {
        // `my_info!` 不是 `info!`。
        let src = "    my_info!(token_hash = %h, \"leak\");\n";
        assert!(identity_leaks(src).is_empty());
    }

    #[test]
    fn a_macro_without_parentheses_is_not_matched() {
        let src = "    tracing::info!(\"just a string\");\n";
        assert!(identity_leaks(src).is_empty());
    }
}
