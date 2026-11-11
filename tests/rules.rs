//! 验证请求头覆写、字节保留与配置输入边界

use cpr_account_headers::{HeaderOverride, Rule};
use gateway_plugin_sdk::call::middleware::MiddlewareHeader;

fn rule(headers: &[(&str, &str)]) -> Rule {
    Rule {
        enabled: true,
        headers: headers
            .iter()
            .map(|(name, value)| HeaderOverride {
                name: (*name).into(),
                value: (*value).into(),
            })
            .collect(),
    }
}

#[test]
fn replaces_all_case_variants_and_preserves_unrelated_bytes() {
    let mut headers = vec![
        MiddlewareHeader {
            name: "User-Agent".into(),
            value: b"old-a".to_vec(),
        },
        MiddlewareHeader {
            name: "x-raw".into(),
            value: vec![0x80, 0xff],
        },
        MiddlewareHeader {
            name: "user-agent".into(),
            value: b"old-b".to_vec(),
        },
        MiddlewareHeader {
            name: "x-raw".into(),
            value: b"second".to_vec(),
        },
    ];
    rule(&[(" User-Agent ", "custom"), ("x-added", "")])
        .normalized()
        .unwrap()
        .apply(&mut headers);
    assert_eq!(headers.len(), 4);
    assert_eq!(headers[0].value, vec![0x80, 0xff]);
    assert_eq!(headers[1].value, b"second");
    assert_eq!(headers[2].name, "user-agent");
    assert_eq!(headers[2].value, b"custom");
    assert_eq!(headers[3].name, "x-added");
    assert!(headers[3].value.is_empty());
}

#[test]
fn disabled_and_empty_rules_keep_exact_headers() {
    let original = vec![MiddlewareHeader {
        name: "X-Original".into(),
        value: vec![0xff],
    }];
    for policy in [
        Rule {
            enabled: false,
            ..rule(&[("x-original", "changed")])
        },
        rule(&[]),
    ] {
        let mut headers = original.clone();
        policy.apply(&mut headers);
        assert!(headers == original);
    }
}

#[test]
fn rejects_injection_duplicates_invalid_names_and_excessive_configuration() {
    for policy in [
        rule(&[("x-a", "safe\r\ninjected: value")]),
        rule(&[("x-a", "nul\0")]),
        rule(&[("bad name", "value")]),
        rule(&[("x-a", "one"), ("X-A", "two")]),
        rule(&[("x-a", &"a".repeat(65_536))]),
        Rule {
            enabled: true,
            headers: (0..65)
                .map(|i| HeaderOverride {
                    name: format!("x-{i}"),
                    value: String::new(),
                })
                .collect(),
        },
    ] {
        assert!(policy.normalized().is_err());
    }
    let valid = rule(&[
        ("authorization", "Bearer synthetic"),
        ("x-value", "  exact value  "),
    ])
    .normalized()
    .unwrap();
    assert_eq!(valid.headers[1].value, "  exact value  ");
}
