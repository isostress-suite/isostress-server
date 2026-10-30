// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

/// 代码侧双向声明：本实现所遵循的文档。
pub const FOLLOWS_DOC: &str = "isostress Protocol Master Guide";

/// 代码侧双向声明：所遵循文档的路径。
// 注意：该路径指向**同级仓库** isostress-docs，在本仓库内不可解析。
pub const FOLLOWS_DOC_PATH: &str = "isostress-docs/en/spec/isostress-1.0.0-Master_Guide.md";

/// 本实现适用的总指导版本（多对多，可多项）。
pub const APPLIES_TO_DOC_VERSIONS: &[&str] = &["1.0.0"];

/// 总指导版本号（仅用于文档修订计数，不参与网络通信）。
pub const DOCS_VERSION: &str = "1.0.0";

/// 本实现的角色。
pub const ROLE: &str = "server";

/// 角色前缀（服务端用角色而非语言）。
pub const ROLE_PREFIX: &str = "SV";

/// 实现版本号。
pub const CODE_VERSION: &str = "1.0.0";

/// 开发版日期段：未进入发布候选的构建带 `-YYYYMMDD`；发布候选与正式发行留空。
/// Manually maintained: updated by hand at commit time, not derived from the commit date.
pub const BUILD_DATE: &str = "20261030";

/// 发布形态的实现标识 `ISOSTRESS_SV-1.0.0`，供兼容清单使用。
pub const IMPLEMENTATION_ID: &str = concat!("ISOSTRESS_", "SV", "-", "1.0.0");

/// 本构建的标识（开发版，含日期段）。clap 的 `version` 需要编译期常量，
/// 因此这里写成字面量；单元测试守住它与 `implementation_id()` 相等。
pub const BUILD_ID: &str = concat!("ISOSTRESS_", "SV", "-", "1.0.0-", "20261030");

/// 组合 `isostress_<前缀>-<版本>[-<YYYYMMDD>]`。
///
/// 空日期返回发布形态；非空日期必须是八位数字，否则直接失败，
/// 不产生一个格式错误的已发布标识。
pub fn implementation_identifier(prefix: &str, code_version: &str, build_date: &str) -> String {
    if build_date.is_empty() {
        return format!("ISOSTRESS_{}-{}", prefix, code_version);
    }
    assert!(
        build_date.len() == 8 && build_date.bytes().all(|b| b.is_ascii_digit()),
        "BUILD_DATE must be YYYYMMDD or empty"
    );
    format!("ISOSTRESS_{}-{}-{}", prefix, code_version, build_date)
}

/// 本构建报告的标识。
pub fn implementation_id() -> String {
    implementation_identifier(ROLE_PREFIX, CODE_VERSION, BUILD_DATE)
}

/// 适用同一总指导版本的客户端版本。
pub const CLIENT_VERSIONS: &[&str] = &["ISOSTRESS_PY-1.0.0", "ISOSTRESS_RS-1.0.0"];

/// 适用同一总指导版本的服务端版本。
pub const SERVER_VERSIONS: &[&str] = &["ISOSTRESS_SV-1.0.0"];

/// CLI `--version` 的单行输出。
pub fn format_version() -> String {
    format!("{} (docs {})", implementation_id(), DOCS_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_contract() {
        assert_eq!(IMPLEMENTATION_ID, "ISOSTRESS_SV-1.0.0");
        assert_eq!(BUILD_DATE, "20261030");
        assert_eq!(BUILD_ID, "ISOSTRESS_SV-1.0.0-20261030");
        assert_eq!(implementation_id(), BUILD_ID);
        assert_eq!(
            implementation_identifier("SV", "1.0.0", ""),
            IMPLEMENTATION_ID
        );
        assert_eq!(DOCS_VERSION, "1.0.0");
        assert_eq!(CODE_VERSION, "1.0.0");
        assert_eq!(ROLE, "server");
        assert_eq!(ROLE_PREFIX, "SV");
    }

    #[test]
    fn bidirectional_declaration_is_present() {
        assert!(!FOLLOWS_DOC.is_empty());
        assert!(FOLLOWS_DOC_PATH.contains("isostress-docs"));
        assert!(!APPLIES_TO_DOC_VERSIONS.is_empty());
        assert!(APPLIES_TO_DOC_VERSIONS.contains(&DOCS_VERSION));
    }

    /// 复用 client 仓的共享一致性向量。
    ///
    /// 派生标签必须与规范逐字一致。
    ///
    /// 规范里 ISOSTRESS-EK-v3 与 ISOSTRESS-EK-ROTATE 是两个不同的串，后者做每包
    /// 旋转，本来就不带 -v3。所以这里不套后缀规则 —— 那条规则会把 EK_ROTATE
    /// 误判成漏了后缀 —— 而是把每个标签的期望值写死，逐字比对。
    #[test]
    fn labels_match_the_spec_verbatim() {
        /// 逐字取自规范 823、894、901 行。
        const EXPECTED: &[(&str, &str)] = &[
            ("INFO_MK", "ISOSTRESS-MK-v3"),
            ("INFO_ANCHOR", "ISOSTRESS-ANCHOR-v3"),
            ("INFO_EK", "ISOSTRESS-EK-v3"),
            ("INFO_SID", "ISOSTRESS-SID-v3"),
            ("INFO_EK_ROTATE", "ISOSTRESS-EK-ROTATE"),
            ("INFO_CONFIRM_INIT", "ISOSTRESS-CONFIRM-INIT-v3"),
            ("INFO_CONFIRM_RESP", "ISOSTRESS-CONFIRM-RESP-v3"),
        ];

        let raw = include_str!("../test-vectors/vectors.json");
        let v: serde_json::Value = serde_json::from_str(raw).expect("vectors.json 必须可解析");
        assert_eq!(v["schema"], "isostress-shared-vectors/1");

        let labels = v["labels"].as_object().expect("labels 必须是对象");
        assert_eq!(
            labels.len(),
            EXPECTED.len(),
            "标签数与规范登记数不符：多一个或少一个都会让双方派生分叉"
        );
        for (name, want) in EXPECTED {
            let got = labels
                .get(*name)
                .and_then(|x| x.as_str())
                .unwrap_or_else(|| panic!("vectors.json 缺少标签 {name}"));
            assert_eq!(got, *want, "{name} 与规范不一致");
        }
    }

    #[test]
    fn format_version_is_single_line() {
        let s = format_version();
        assert!(s.starts_with("ISOSTRESS_SV-1.0.0-20261030"));
        assert!(!s.contains('\n'));
    }
}
