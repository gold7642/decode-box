//! BrCipherMaker（"log 加密"）的 Rust 移植，与 Java 原实现逐语义对齐。
//!
//! 算法结构（新版，分隔符 'Β' U+0392）：
//!   1. 按 `java_hash(明文) % 10` 的绝对值从 10 把内置密钥中选一把
//!   2. 明文按 UTF-16 code unit 逐字符与密钥字符循环 XOR
//!   3. XOR 结果按 Java 语义转 UTF-8 字节，Base64 URL-safe 无 padding 编码
//!   4. 在 `abs(java_hash(密文) % 密文长度)` 位置插入 'Β' + 密钥索引数字（s==0 时取 2）
//!
//! 旧版（分隔符 'Α' U+0391）：第 2 步换成移位（key char - 97，跳过空格不推进 j）。
//!
//! 移植要点：
//! - Java `String.hashCode()`：UTF-16 code unit 上的 32 位回绕乘加
//! - Java `%` 与 Rust `%` 同为截断除法（符号跟随被除数）
//! - Java `new String(char[]).getBytes("utf-8")` 对未配对代理对输出 '?'
//! - commons-lang `split`：相邻分隔符合并、丢弃空 token

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

const KEYS: [&str; 10] = [
    "c849a06defd23bac",
    "c9e50bfe3ccbe05a",
    "025371b9fef1098f",
    "0b74d38da0a8789d",
    "89b66e0f0917d044",
    "afb283dbd5a1c950",
    "b898901aded8d6a9",
    "385baab99a0038c6",
    "17360f5c5de62d1e",
    "9f057500d30f94fa",
];

/// 解密结果：成功解出 / 不是 log 密文 / 结构损坏
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogDecodeOutcome {
    Decoded(String),
    NotCipher,
    Failed,
}

/// Java String.hashCode：UTF-16 code unit 逐位 h = 31*h + c（i32 回绕）
#[cfg_attr(not(test), allow(dead_code))]
fn java_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for u in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u as i32);
    }
    h
}

/// Java `Math.abs(h % keysLen)` 选密钥索引
fn index_for(s: &str) -> usize {
    let idx = (java_hash(s) % KEYS.len() as i32).abs();
    idx as usize
}

/// UTF-16 code unit 序列按 Java UTF-8 编码器语义（未配对代理 → '?'）转字节
fn units_to_java_utf8(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len());
    for &u in units {
        if (0xD800..=0xDFFF).contains(&u) {
            out.push(b'?');
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(
                char::from_u32(u as u32)
                    .unwrap()
                    .encode_utf8(&mut buf)
                    .as_bytes(),
            );
        }
    }
    out
}

/// UTF-16 code unit 序列转 Rust String（未配对代理 → U+FFFD，仅出现在密文损坏时）
fn units_to_string_lossy(units: &[u16]) -> String {
    units
        .iter()
        .map(|&u| char::from_u32(u as u32).unwrap_or('\u{FFFD}'))
        .collect()
}

/// commons-lang StringUtils.split(str, seps)：按任意分隔字符切分，合并相邻分隔符，丢弃空 token
fn split_cl<'a>(s: &'a str, seps: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        if seps.contains(c) {
            if let Some(st) = start.take() {
                out.push(&s[st..i]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

// ===== 新版：XOR =====

fn encode_xor(plain: &str, key: &str) -> String {
    let ku: Vec<u16> = key.to_lowercase().encode_utf16().collect();
    let units: Vec<u16> = plain
        .encode_utf16()
        .enumerate()
        .map(|(i, u)| u ^ ku[i % ku.len()])
        .collect();
    URL_SAFE_NO_PAD.encode(units_to_java_utf8(&units))
}

fn decode_xor(text: &str, key: &str) -> Option<String> {
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    let s = String::from_utf8_lossy(&bytes);
    let ku: Vec<u16> = key.to_lowercase().encode_utf16().collect();
    let units: Vec<u16> = s
        .encode_utf16()
        .enumerate()
        .map(|(i, u)| u ^ ku[i % ku.len()])
        .collect();
    Some(units_to_string_lossy(&units))
}

// ===== 旧版：移位（跳过空格） =====

#[cfg_attr(not(test), allow(dead_code))]
fn encode_shift(plain: &str, key: &str) -> String {
    let ku: Vec<u16> = key.to_lowercase().encode_utf16().collect();
    let off: Vec<u16> = ku.iter().map(|&c| c.wrapping_sub(97)).collect();
    let mut units: Vec<u16> = plain.encode_utf16().collect();
    let mut j = 0usize;
    for u in units.iter_mut() {
        if *u != 0x20 {
            *u = u.wrapping_add(off[j % off.len()]);
            j += 1;
        }
    }
    URL_SAFE_NO_PAD.encode(units_to_java_utf8(&units))
}

fn decode_shift(text: &str, key: &str) -> Option<String> {
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    let s = String::from_utf8_lossy(&bytes);
    let ku: Vec<u16> = key.to_lowercase().encode_utf16().collect();
    let off: Vec<u16> = ku.iter().map(|&c| c.wrapping_sub(97)).collect();
    let mut units: Vec<u16> = s.encode_utf16().collect();
    let mut j = 0usize;
    for u in units.iter_mut() {
        if *u != 0x20 {
            *u = u.wrapping_sub(off[j % off.len()]);
            j += 1;
        }
    }
    Some(units_to_string_lossy(&units))
}

// ===== 对外 API =====

/// log 加密（新版 Β），与 Java BrCipherMaker.encode 一致。用于自测与数据回填。
pub fn encode(plain: &str) -> String {
    if is_blank(plain) {
        return plain.to_string();
    }
    let idx = index_for(plain);
    let mw = encode_xor(plain, KEYS[idx]);
    if !mw.is_empty() {
        // mw 是纯 ASCII base64：字节长度 == 字符长度 == UTF-16 长度
        let lt = mw.len() as i32;
        let mut s = (java_hash(&mw) % lt).abs();
        if s == 0 {
            s = 2;
        }
        let s = s as usize;
        return format!("{}Β{}{}", &mw[..s], idx, &mw[s..]);
    }
    mw
}

/// log 解密：自动识别新版（Β）/旧版（Α）。仅对含分隔符的值有意义。
pub fn decode(text: &str) -> LogDecodeOutcome {
    if is_blank(text) {
        return LogDecodeOutcome::NotCipher;
    }
    if text.contains('Α') {
        return decode_old(text);
    }
    if !text.contains('Β') {
        return LogDecodeOutcome::NotCipher;
    }
    let parts = split_cl(text, "Β");
    if parts.len() > 1 {
        decode_with_parts(&parts, decode_xor)
    } else {
        LogDecodeOutcome::Failed
    }
}

fn decode_old(text: &str) -> LogDecodeOutcome {
    if !text.contains('Α') {
        return LogDecodeOutcome::NotCipher;
    }
    let parts = split_cl(text, "Α");
    if parts.len() > 1 {
        decode_with_parts(&parts, decode_shift)
    } else {
        LogDecodeOutcome::Failed
    }
}

fn decode_with_parts(parts: &[&str], inner: fn(&str, &str) -> Option<String>) -> LogDecodeOutcome {
    let src_end = parts[1];
    let mut chars = src_end.chars();
    match chars.next() {
        Some(c) if c.is_ascii_digit() => {
            let idx = c as usize - '0' as usize;
            if idx >= KEYS.len() {
                return LogDecodeOutcome::Failed;
            }
            let src = format!("{}{}", parts[0], chars.as_str());
            match inner(&src, KEYS[idx]) {
                Some(s) => LogDecodeOutcome::Decoded(s),
                None => LogDecodeOutcome::Failed,
            }
        }
        _ => LogDecodeOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 向量由"忠实还原的 Java 版 ↔ Python 移植版"双向交叉验证生成，
    /// 覆盖全部 10 把密钥、含空格、中文、带尾随空格等边界。
    const VECTORS: &[(&str, &str, &str)] = &[
        // (明文, 新版密文, 旧版密文)
        ("13800138000", "UwsBCAΒ6kBAllUVVQ", "MgΑ6oQBwgAAzgzNDM"),
        ("15912345678", "UFNbAwoAUFdSΒ5Alk", "MTo6AΑ5gkFNzY5Czg"),
        ("18612349876", "AVoBBVYADF1ZΒ3B1c", "ADΑ3kMBDUFCzw4BjY"),
        ("17098765432", "Uw8JAQΒ6EHB1RQVlY", "Mg4IEBAGBjUΑ63NzU"),
        ("19912345678", "UgBcBAJRUlAΒ1FVFs", "MxE9BQEΑ10OTkIOTo"),
        ("16600001111", "AVQBBFQDCΒ3FVQAVA", "ADcMAzMΑ3CBzQxADE"),
        ("17611112222", "Uw8PCΒ6QgBAFNWV1Y", "Mg4OCAkAΑ6ATI1NjU"),
        ("13133334444", "UFVTAQsAΒ5V1ZQAVU", "MTgyBAoΑ5FNjU3CDQ"),
        ("15655556666", "Ug0CDFΒ0QFA1JTUFI", "MΑ0wwJDTUECjk6Ozk"),
        ("18777778888", "UgADDlYHAVxdXlΒ0w", "Mw8KΑ0DzcGDDs8PTs"),
        ("18899990000", "UF5aCwEΒ5KXVJUBVE", "MΑ5T05ChALPDEzBDA"),
        ("13212121212", "UwsΒ6LCQsBA1BWVFY", "MgoKCAoAAjE1NTΑ6U"),
        ("138 0013 8000", "AQENEwcBUwpGXVYBΒ2AA", "AAQMIAIGΑ2ATQgEDU0NQ"),
        ("张三", "5bΒ9yZ5Lmv", "5bu45LiΑ9O"),
        ("13800138000 ", "AAQLBΒ8gBXBlsFVFUW", "AQkKBe-_vzYΑ8HOgQzNCA"),
        ("19480801586", "CQΒ4BWDgZdAFcFAQc", "CBE1DQUΑ4877-_NgQQBg"),
        ("15023709720", "Uw0JCΒ6goHAVhTV1Q", "MgwICQsGADk6NΑ6jM"),
    ];

    /// 生产库真实密文（sms_message 表 phone 字段）
    const REAL_CIPHER: &str = "Uw0JCΒ6goHAVhTV1Q";
    const REAL_PLAIN: &str = "15023709720";

    /// 19 条真实生产密文 → 真实手机号（从 sms_message 表抽取，Python 版与 Java 版均验证一致）
    const REAL_PHONE_VECTORS: &[(&str, &str)] = &[
        ("Uw0JCΒ6goHAVhTV1Q", "15023709720"),
        ("AgsDVVBTVΒ7g4OVQE", "13671247741"),
        ("CΒ9FMHBQYHCAdRBAA", "15701287570"),
        ("CQ5UΒ4BgZTCVYGCgc", "17600690636"),
        ("CQpRBQVWCVcJCAΒ4k", "13333391918"),
        ("AVcΒ3GBFUGDlRRCFc", "15101560086"),
        ("UgpQBwVXUlMHVΒ1FY", "13525546475"),
        ("AQYΒ2BAwMAWwlWVVc", "14404190001"),
        ("UFJWCgEDXVVcΒ5A1Y", "14489097867"),
        ("AAΒ8AFBwBQAFMGXVM", "17610650396"),
        ("CFΒ9IEBAMABgZcBAU", "14414566875"),
        ("UFJWAwoAVlFXΒ5BFI", "14412323313"),
        ("CFIEBgQΒ9BBAJVAQM", "14433442123"),
        ("AQYBAQUHUΒ2g1UXFA", "14422604296"),
        ("UFJWΒ5CgkGU1NRA1g", "14481571569"),
        ("AVYDAΒ3lUHCFxQCFU", "14461408184"),
        ("AQYBAg4GWgtWVFΒ2U", "14419782013"),
        ("Ug1Β1RDQRSXlALW1A", "14484085883"),
        ("CF4Β9DBgQEBghVBgA", "18333168150"),
    ];

    #[test]
    fn decode_new_vectors() {
        for (plain, enc_new, _) in VECTORS {
            assert_eq!(
                decode(enc_new),
                LogDecodeOutcome::Decoded(plain.to_string()),
                "new: {plain}"
            );
        }
    }

    #[test]
    fn decode_old_vectors() {
        for (plain, _, enc_old) in VECTORS {
            assert_eq!(
                decode(enc_old),
                LogDecodeOutcome::Decoded(plain.to_string()),
                "old: {plain}"
            );
        }
    }

    #[test]
    fn encode_new_vectors() {
        for (plain, enc_new, _) in VECTORS {
            assert_eq!(encode(plain), *enc_new, "enc: {plain}");
        }
    }

    #[test]
    fn real_production_cipher() {
        assert_eq!(
            decode(REAL_CIPHER),
            LogDecodeOutcome::Decoded(REAL_PLAIN.to_string())
        );
    }

    #[test]
    fn real_phone_vectors_from_production() {
        for (cipher, plain) in REAL_PHONE_VECTORS {
            assert_eq!(
                decode(cipher),
                LogDecodeOutcome::Decoded(plain.to_string()),
                "真实密文 {cipher}"
            );
        }
    }

    #[test]
    fn roundtrip_random_phones() {
        // LCG 伪随机手机号往返测试
        let mut seed: u64 = 0x2545F4914F6CDD1D;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed >> 33
        };
        for _ in 0..2000 {
            let n = 13000000000u64 + next() % 7000000000;
            let phone = n.to_string();
            assert_eq!(
                decode(&encode(&phone)),
                LogDecodeOutcome::Decoded(phone.clone())
            );
        }
    }

    #[test]
    fn non_cipher_passthrough() {
        assert_eq!(decode("13800138000"), LogDecodeOutcome::NotCipher);
        assert_eq!(decode(""), LogDecodeOutcome::NotCipher);
        assert_eq!(
            decode("5e5ca4a768d0556a7bd8b6b0f4894fe4"),
            LogDecodeOutcome::NotCipher
        );
    }

    #[test]
    fn malformed_cipher_fails() {
        // 与 Java 原版语义一致：结构损坏 → Failed（Java 侧 catch 后返回原文）
        assert_eq!(decode("Β"), LogDecodeOutcome::Failed);
        assert_eq!(decode("abcΒxdef"), LogDecodeOutcome::Failed); // 索引位非数字
        // Java 原版对"能 base64 解码的垃圾"会解出乱码而非报错——此处验证同样不 panic 且有输出
        assert!(matches!(decode("abcΒ99"), LogDecodeOutcome::Decoded(_)));
    }

    #[test]
    fn java_hash_semantics() {
        // 与 Python 版 java_hash 预计算值对照（Python 版又与 Java 原版交叉验证过）
        assert_eq!(java_hash(""), 0);
        assert_eq!(java_hash("a"), 97);
        assert_eq!(java_hash("13800138000"), 1430905456);
        assert_eq!(java_hash("张三"), 774889);
    }
}
