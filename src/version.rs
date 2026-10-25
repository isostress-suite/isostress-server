// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 ZT-DSP Suite contributors

/// 代码侧双向声明：本实现所遵循的文档。
pub const FOLLOWS_DOC: &str = "ZT-DSP Protocol Master Guide";

/// 代码侧声明：所遵循文档的路径。
// 注意：该路径指向**同级仓库** ZT-DSP-docs，在本仓库内不可解析。
pub const FOLLOWS_DOC_PATH: &str = "ZT-DSP-docs/en/spec/ZT-DSP-1.0.2-Master_Guide.md";

/// 总指导版本号（仅用于文档修订计数，不参与网络通信）。
pub const DOCS_VERSION: &str = "1.0.2";

/// 本实现的角色。
pub const ROLE: &str = "server";

/// 角色前缀（服务端用角色而非语言）。
pub const ROLE_PREFIX: &str = "SV";

/// 实现版本号。
pub const CODE_VERSION: &str = "1.0.0";

/// 实现标识 `ZT-DSP_SV-1.0.0`。
pub const IMPLEMENTATION_ID: &str = concat!("ZT-DSP_", "SV", "-", "1.0.0");

/// 适用同一总指导版本的客户端版本。
pub const CLIENT_VERSIONS: &[&str] = &["ZT-DSP_PY-1.0.0", "ZT-DSP_RS-1.0.0"];

/// 适用同一总指导版本的服务端版本。
pub const SERVER_VERSIONS: &[&str] = &["ZT-DSP_SV-1.0.0"];

/// CLI `--version` 的单行输出。
pub fn format_version() -> String {
    format!("{} (docs {})", IMPLEMENTATION_ID, DOCS_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_contract() {
        assert_eq!(IMPLEMENTATION_ID, "ZT-DSP_SV-1.0.0");
        assert_eq!(DOCS_VERSION, "1.0.2");
        assert_eq!(CODE_VERSION, "1.0.0");
        assert_eq!(ROLE, "server");
        assert_eq!(ROLE_PREFIX, "SV");
    }

    #[test]
    fn bidirectional_declaration_is_present() {
        assert!(!FOLLOWS_DOC.is_empty());
        assert!(FOLLOWS_DOC_PATH.contains("ZT-DSP-docs"));
        assert!(!DOCS_VERSION.is_empty());
    }

    /// 复用 client 仓的共享一致性向量。
    ///
    /// 派生标签必须与规范逐字一致。
    ///
    /// 规范里 ZT-DSP-EK-v3 与 ZT-DSP-EK-ROTATE 是两个不同的串，后者做每包
    /// 旋转，本来就不带 -v3。所以这里不套后缀规则 —— 那条规则会把 EK_ROTATE
    /// 误判成漏了后缀 —— 而是把每个标签的期望值写死，逐字比对。
    #[test]
    fn labels_match_the_spec_verbatim() {
        /// 逐字取自规范 823、894、901 行。
        const EXPECTED: &[(&str, &str)] = &[
            ("INFO_MK", "ZT-DSP-MK-v3"),
            ("INFO_ANCHOR", "ZT-DSP-ANCHOR-v3"),
            ("INFO_EK", "ZT-DSP-EK-v3"),
            ("INFO_SID", "ZT-DSP-SID-v3"),
            ("INFO_EK_ROTATE", "ZT-DSP-EK-ROTATE"),
            ("INFO_CONFIRM_INIT", "ZT-DSP-CONFIRM-INIT-v3"),
            ("INFO_CONFIRM_RESP", "ZT-DSP-CONFIRM-RESP-v3"),
        ];

        let raw = include_str!("../test-vectors/vectors.json");
        let v: serde_json::Value = serde_json::from_str(raw).expect("vectors.json 必须可解析");
        assert_eq!(v["schema"], "zt-dsp-shared-vectors/1");

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
        assert!(s.starts_with("ZT-DSP_SV-1.0.0"));
        assert!(!s.contains('\n'));
    }
}
