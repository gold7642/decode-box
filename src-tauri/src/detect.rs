//! 密文类型自动识别：逐值判定 log / md5 / sha256 / 明文手机号 / 未知。

use crate::cipher;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CipherKind {
    Log,
    Md5,
    Sha256,
    PlainPhone,
    Empty,
    Unknown,
}

impl CipherKind {
    pub fn label(&self) -> &'static str {
        match self {
            CipherKind::Log => "log 密文",
            CipherKind::Md5 => "md5",
            CipherKind::Sha256 => "sha256",
            CipherKind::PlainPhone => "明文手机号",
            CipherKind::Empty => "空值",
            CipherKind::Unknown => "无法识别",
        }
    }
}

/// 中国大陆手机号：11 位、1 开头、第二位 3-9（与 PhoneUtil.MAINLAND_CHINA_MOBILE 一致）
pub fn is_mainland_phone(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 11
        && b[0] == b'1'
        && (b'3'..=b'9').contains(&b[1])
        && b[2..].iter().all(|c| c.is_ascii_digit())
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// 单值密文类型判定（先 trim，与 Java 调用侧 .trim() 习惯一致）
pub fn detect(value: &str) -> CipherKind {
    let v = value.trim();
    if v.is_empty() {
        return CipherKind::Empty;
    }
    if v.contains('Β') || v.contains('Α') {
        // log 密文；顺带校验能否成功解出（用于更准确的列统计）
        return match cipher::decode(v) {
            cipher::LogDecodeOutcome::Decoded(_) => CipherKind::Log,
            _ => CipherKind::Unknown,
        };
    }
    if v.len() == 32 && is_hex(v) {
        return CipherKind::Md5;
    }
    if v.len() == 64 && is_hex(v) {
        return CipherKind::Sha256;
    }
    if is_mainland_phone(v) {
        return CipherKind::PlainPhone;
    }
    CipherKind::Unknown
}
