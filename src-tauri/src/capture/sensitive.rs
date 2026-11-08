//! A08 第二层：按内容识别高置信的密钥和 Token。只认有固定前缀或固定结构的格式，
//! 宁可漏掉，不误伤普通文本；每条规则都可以在设置里单独关闭。

use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
pub struct Rule {
    pub id: &'static str,
    pub label: &'static str,
    pub example: &'static str,
    #[serde(skip)]
    matches: fn(&str) -> bool,
}

pub const RULES: &[Rule] = &[
    Rule {
        id: "private_key",
        label: "私钥",
        example: "-----BEGIN … PRIVATE KEY-----（PEM、OpenSSH、PGP）",
        matches: private_key,
    },
    Rule {
        id: "github_token",
        label: "GitHub / GitLab Token",
        example: "ghp_…、github_pat_…、glpat-…",
        matches: github_token,
    },
    Rule {
        id: "ai_api_key",
        label: "AI 服务密钥",
        example: "sk-ant-…、sk-proj-…",
        matches: ai_api_key,
    },
    Rule {
        id: "cloud_access_key",
        label: "云服务访问密钥",
        example: "AWS AKIA…、阿里云 LTAI…、腾讯云 AKID…、Google AIza…",
        matches: cloud_access_key,
    },
    Rule {
        id: "service_token",
        label: "Slack / Stripe 密钥",
        example: "xoxb-…、sk_live_…",
        matches: service_token,
    },
    Rule {
        id: "jwt",
        label: "JWT 令牌",
        example: "eyJ….eyJ….…",
        matches: jwt,
    },
];

/// 返回命中的第一条未关闭规则。
pub fn detect(text: &str, disabled: &[String]) -> Option<&'static str> {
    RULES
        .iter()
        .filter(|rule| !disabled.iter().any(|id| id == rule.id))
        .find(|rule| (rule.matches)(text))
        .map(|rule| rule.id)
}

fn alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}
fn upper_digit(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}
fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
fn url_safe(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// 任意一处 `prefix` 后紧跟一段字符：前面不是 Token 字符（避免嵌在长单词里），
/// 后面整段都属于 `charset`，长度在 [min, max] 之间（max 为 None 时只要求至少 min）。
fn token(
    text: &str,
    prefix: &str,
    charset: fn(char) -> bool,
    min: usize,
    max: Option<usize>,
) -> bool {
    text.match_indices(prefix).any(|(start, _)| {
        if text[..start].chars().next_back().is_some_and(url_safe) {
            return false;
        }
        let run = text[start + prefix.len()..]
            .chars()
            .take_while(|c| charset(*c))
            .take(max.map_or(usize::MAX, |max| max + 1))
            .count();
        run >= min && max.is_none_or(|max| run <= max)
    })
}

fn private_key(text: &str) -> bool {
    text.match_indices("-----BEGIN ").any(|(start, matched)| {
        let rest = &text[start + matched.len()..];
        let Some(end) = rest
            .get(..rest.len().min(48))
            .and_then(|head| head.find("-----"))
        else {
            return false;
        };
        let label = &rest[..end];
        label.ends_with("PRIVATE KEY") || label.ends_with("PRIVATE KEY BLOCK")
    })
}

fn github_token(text: &str) -> bool {
    ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"]
        .iter()
        .any(|prefix| token(text, prefix, alnum, 36, None))
        || token(text, "github_pat_", word, 70, None)
        || token(text, "glpat-", url_safe, 20, None)
}

fn ai_api_key(text: &str) -> bool {
    token(text, "sk-ant-", url_safe, 40, None) || token(text, "sk-proj-", url_safe, 40, None)
}

fn cloud_access_key(text: &str) -> bool {
    token(text, "AKIA", upper_digit, 16, Some(16))
        || token(text, "ASIA", upper_digit, 16, Some(16))
        || token(text, "LTAI", alnum, 12, Some(30))
        || token(text, "AKID", alnum, 32, Some(40))
        || token(text, "AIza", url_safe, 35, Some(35))
}

fn service_token(text: &str) -> bool {
    ["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"]
        .iter()
        .any(|prefix| token(text, prefix, |c| alnum(c) || c == '-', 20, None))
        || token(text, "sk_live_", alnum, 20, None)
        || token(text, "rk_live_", alnum, 20, None)
}

fn jwt(text: &str) -> bool {
    // 三段 base64url，用点分隔：头和载荷都是 JSON（eyJ 开头），每段至少 10 个字符。
    let run = |s: &str| s.bytes().take_while(|b| url_safe(*b as char)).count();
    text.match_indices("eyJ").any(|(start, _)| {
        if text[..start].chars().next_back().is_some_and(url_safe) {
            return false;
        }
        let header = &text[start..];
        let length = run(header);
        let Some(payload) = header[length..].strip_prefix('.') else {
            return false;
        };
        let payload_length = run(payload);
        let Some(signature) = payload[payload_length..].strip_prefix('.') else {
            return false;
        };
        length >= 10 && payload.starts_with("eyJ") && payload_length >= 10 && run(signature) >= 10
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(text: &str) -> Option<&'static str> {
        detect(text, &[])
    }

    #[test]
    fn recognizes_high_confidence_secrets() {
        let cases = [
            (
                "-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbn...\n-----END OPENSSH PRIVATE KEY-----",
                "private_key",
            ),
            ("key:\n-----BEGIN RSA PRIVATE KEY-----\nMIIE", "private_key"),
            ("-----BEGIN PGP PRIVATE KEY BLOCK-----", "private_key"),
            (
                "token=ghp_0123456789abcdefghijABCDEFGHIJ012345",
                "github_token",
            ),
            (&format!("github_pat_{}", "A1_".repeat(25)), "github_token"),
            ("glpat-abcdefghij0123456789", "github_token"),
            (
                &format!("ANTHROPIC_API_KEY=sk-ant-api03-{}", "x".repeat(60)),
                "ai_api_key",
            ),
            (&format!("sk-proj-{}", "Ab9_".repeat(12)), "ai_api_key"),
            (
                "aws_access_key_id = AKIAIOSFODNN7EXAMPLE",
                "cloud_access_key",
            ),
            ("LTAI5tAbCdEfGhIjKlMnOp", "cloud_access_key"),
            (&format!("AKID{}", "a1".repeat(18)), "cloud_access_key"),
            (&format!("AIza{}", "B".repeat(35)), "cloud_access_key"),
            ("xoxb-123456789012-abcdefghijkl", "service_token"),
            (
                &format!("sk_live_{}", "4eC39HqLyjWDarjtT1zdp7dc"),
                "service_token",
            ),
            (
                "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
                "jwt",
            ),
        ];
        for (text, rule) in cases {
            assert_eq!(hit(text), Some(rule), "{text}");
        }
    }

    #[test]
    fn ordinary_text_passes() {
        for text in [
            "",
            "你好，世界",
            "-----BEGIN CERTIFICATE-----\nMIID\n-----END CERTIFICATE-----",
            "-----BEGIN PUBLIC KEY-----",
            "ghp_short",
            "AKIA 太短 AKIA123",
            "AKIAIOSFODNN7EXAMPLEXX",
            "MAKIAIOSFODNN7EXAMPLE",
            "the task-proj- plan",
            "sk-ant-short",
            "eyJ only.one",
            "version 1.2.3, see https://example.com/eyJ",
            "xoxb-short",
        ] {
            assert_eq!(hit(text), None, "{text}");
        }
    }

    #[test]
    fn disabled_rules_are_skipped() {
        let text = "AKIAIOSFODNN7EXAMPLE";
        assert_eq!(detect(text, &["cloud_access_key".into()]), None);
        assert_eq!(detect(text, &["jwt".into()]), Some("cloud_access_key"));
    }

    #[test]
    fn rule_ids_are_unique() {
        let mut ids: Vec<_> = RULES.iter().map(|rule| rule.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), RULES.len());
    }
}
