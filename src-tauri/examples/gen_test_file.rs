//! 测试数据生成 + 端到端验证（CLI）。
//!
//! 用法：
//!   cargo run --example gen_test_file -- gen <rows> <out.csv>      # 生成混合密文 CSV + 明文对照
//!   cargo run --example gen_test_file -- verify <in.csv> <truth>   # 端到端解密 + 准确率比对
//!
//! 生成策略：log 用随机手机号（本地可解）；md5/sha256 用真实手机号 15023709720 的摘要
//! （真实服务可查，用于验证远程查表正确性）掺杂少量随机摘要（验证"查无映射"路径）。

use std::env;
use std::fs;
use std::io::Write;
use std::time::Instant;

use phone_decrypt_tool_lib::cipher;
use phone_decrypt_tool_lib::detect::{self, CipherKind};
use phone_decrypt_tool_lib::grpc_client::{DecodeClient, GrpcConfig};
use tokio_util::sync::CancellationToken;

/// 真实服务中确有映射的手机号及其 md5/sha256（此前用 Java 客户端验证过）
const REAL_PHONE: &str = "15023709720";
const REAL_MD5: &str = "5e5ca4a768d0556a7bd8b6b0f4894fe4";
const REAL_SHA256: &str = "eda7929d0367c39da8b25af004b1f59bbe30e5b52572c591f3456cf0498e13a7";

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

fn random_phone(rng: &mut Lcg) -> String {
    let second = b'3' + (rng.next() % 7) as u8;
    let mut p = String::with_capacity(11);
    p.push('1');
    p.push(second as char);
    for _ in 0..9 {
        p.push((b'0' + (rng.next() % 10) as u8) as char);
    }
    p
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen") => {
            let rows: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(200_000);
            let out = args.get(3).cloned().unwrap_or_else(|| "/tmp/dctest/test_200k.csv".into());
            gen(rows, &out);
        }
        Some("verify") => {
            let input = args.get(2).cloned().expect("需要输入 CSV");
            let truth = args.get(3).cloned().expect("需要 ground truth CSV");
            let column: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            verify(&input, &truth, column);
        }
        _ => eprintln!("用法: gen <rows> <out.csv> | verify <in.csv> <truth> [column]"),
    }
}

fn gen(rows: usize, out: &str) {
    let mut rng = Lcg(0x20260901);
    let truth_path = out.replace(".csv", "_truth.csv");
    let mut file = fs::File::create(out).expect("创建文件失败");
    let mut wtr = csv::Writer::from_writer(file);
    // 单列文件：真实场景只有 phone_encrypted 一列
    wtr.write_record(["phone_encrypted"]).unwrap();

    let mut tfile = fs::File::create(&truth_path).expect("创建 truth 失败");
    let mut twtr = csv::Writer::from_writer(tfile);
    twtr.write_record(["row_no", "kind", "plain"]).unwrap();

    let mut dist = [0usize; 5]; // log md5 sha plain dirty
    for i in 0..rows {
        let r = rng.next() % 100;
        let (enc, kind, plain) = if r < 50 {
            let p = random_phone(&mut rng);
            dist[0] += 1;
            (cipher::encode(&p), "log", p)
        } else if r < 65 {
            dist[1] += 1;
            // 90% 真实映射 + 10% 随机（查无映射）
            if rng.next() % 10 < 9 {
                (REAL_MD5.to_string(), "md5", REAL_PHONE.to_string())
            } else {
                let p = random_phone(&mut rng);
                (md5_hex(p.as_bytes()), "md5", p)
            }
        } else if r < 78 {
            dist[2] += 1;
            if rng.next() % 10 < 9 {
                (REAL_SHA256.to_string(), "sha256", REAL_PHONE.to_string())
            } else {
                let p = random_phone(&mut rng);
                (sha256_hex(p.as_bytes()), "sha256", p)
            }
        } else if r < 98 {
            let p = random_phone(&mut rng);
            dist[3] += 1;
            (p.clone(), "plain", p)
        } else {
            // 无法识别的脏数据（验证失败文件）
            dist[4] += 1;
            (format!("invalid_data_{:06}", i), "dirty", String::new())
        };
        wtr.write_record([enc]).unwrap();
        twtr.write_record([i.to_string(), kind.to_string(), plain]).unwrap();
    }
    wtr.flush().unwrap();
    twtr.flush().unwrap();
    let size = fs::metadata(out).unwrap().len();
    println!(
        "生成 {rows} 行 -> {out} ({:.1} MB)，明文对照 -> {truth_path}",
        size as f64 / 1048576.0
    );
    println!("分布: log={} md5={} sha256={} plain={} dirty={}", dist[0], dist[1], dist[2], dist[3], dist[4]);
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn verify(input: &str, truth: &str, column: usize) {
    let start = Instant::now();
    let bytes = fs::read(input).expect("读取失败");
    println!("读取 {} ({:.1} MB): {:?}", input, bytes.len() as f64 / 1048576.0, start.elapsed());

    let t = Instant::now();
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(true)
        .from_reader(bytes.as_slice());
    let mut rows: Vec<Vec<String>> = Vec::new();
    for rec in rdr.records() {
        rows.push(rec.unwrap().iter().map(|s| s.to_string()).collect());
    }
    let data_rows = rows.len();
    println!("解析 CSV {data_rows} 行: {:?}", t.elapsed());

    // 分类 + 收集远程 key
    let t = Instant::now();
    let mut outcomes: Vec<(CipherKind, String)> = Vec::with_capacity(data_rows);
    let mut md5_keys = std::collections::HashSet::new();
    let mut sha_keys = std::collections::HashSet::new();
    for row in &rows {
        let raw = row.get(column).cloned().unwrap_or_default();
        match detect::detect(&raw) {
            CipherKind::Log => outcomes.push((CipherKind::Log, raw)),
            CipherKind::Md5 => {
                md5_keys.insert(raw.trim().to_string());
                outcomes.push((CipherKind::Md5, raw));
            }
            CipherKind::Sha256 => {
                sha_keys.insert(raw.trim().to_string());
                outcomes.push((CipherKind::Sha256, raw));
            }
            CipherKind::PlainPhone => outcomes.push((CipherKind::PlainPhone, raw)),
            CipherKind::Empty => outcomes.push((CipherKind::Empty, raw)),
            CipherKind::Unknown => outcomes.push((CipherKind::Unknown, raw)),
        }
    }
    println!(
        "分类完成: {:?} (md5 unique={}, sha unique={})",
        t.elapsed(),
        md5_keys.len(),
        sha_keys.len()
    );

    // 远程查表
    let mut remote_map: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
    if !md5_keys.is_empty() || !sha_keys.is_empty() {
        let t = Instant::now();
        let client = DecodeClient::connect(GrpcConfig::default()).await.expect("连接失败");
        let cancel = CancellationToken::new();
        if !md5_keys.is_empty() {
            let (res, errs) = client
                .decode_batch(md5_keys.into_iter().collect(), "md5", &cancel, |_, _| {})
                .await;
            println!("md5 远程查表 {} 条: {:?} (err={})", res.len(), t.elapsed(), errs.len());
            remote_map.extend(res);
        }
        if !sha_keys.is_empty() {
            let (res, errs) = client
                .decode_batch(sha_keys.into_iter().collect(), "sha", &cancel, |_, _| {})
                .await;
            println!("sha256 远程查表 {} 条: {:?} (err={})", res.len(), t.elapsed(), errs.len());
            remote_map.extend(res);
        }
    }

    // 解密汇总 + 与 truth 比对
    let t = Instant::now();
    let mut trdr = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(true)
        .from_path(truth)
        .expect("读 truth 失败");
    let truths: Vec<(String, String)> = trdr
        .records()
        .map(|r| {
            let r = r.unwrap();
            (r[1].to_string(), r[2].to_string())
        })
        .collect();

    let (mut ok, mut mismatch, mut fail) = (0usize, 0usize, 0usize);
    let mut out_rows: Vec<Vec<String>> = Vec::with_capacity(data_rows);
    for (i, row) in rows.iter().enumerate() {
        let raw = row.get(column).cloned().unwrap_or_default();
        let (kind, _) = &outcomes[i];
        let decoded = match kind {
            CipherKind::Log => match cipher::decode(&raw) {
                cipher::LogDecodeOutcome::Decoded(s) => s,
                _ => String::new(),
            },
            CipherKind::Md5 | CipherKind::Sha256 => remote_map
                .get(raw.trim())
                .and_then(|v| v.clone())
                .unwrap_or_default(),
            CipherKind::PlainPhone => raw.trim().to_string(),
            _ => String::new(),
        };
        let expected = &truths.get(i).map(|t| t.1.as_str()).unwrap_or("");
        if kind == &CipherKind::Empty {
            ok += 1;
        } else if decoded == *expected && !decoded.is_empty() {
            ok += 1;
        } else if kind == &CipherKind::PlainPhone && decoded == *expected {
            ok += 1;
        } else if kind == &CipherKind::Unknown {
            fail += 1;
        } else {
            mismatch += 1;
            if mismatch <= 5 {
                println!("  不匹配 行{i}: kind={:?} 期望={expected} 实得={decoded}", kind);
            }
        }
        let mut r = row.clone();
        r.insert(column + 1, decoded);
        out_rows.push(r);
    }
    println!(
        "解密+比对: {:?} (ok={ok} mismatch={mismatch} fail={fail})",
        t.elapsed()
    );

    // 写出解密结果
    let t = Instant::now();
    let out = input.replace(".csv", "_decrypted.csv");
    let mut file = fs::File::create(&out).expect("创建输出失败");
    file.write_all(&[0xEF, 0xBB, 0xBF]).unwrap();
    let mut wtr = csv::WriterBuilder::new().from_writer(file);
    wtr.write_record(["user_id", "phone_encrypted", "解密结果", "masked"]).unwrap();
    for r in &out_rows {
        wtr.write_record(r).unwrap();
    }
    wtr.flush().unwrap();
    println!("写出 {data_rows} 行: {:?} -> {out}", t.elapsed());
    println!(
        "====== 总耗时 {:?}，准确率 {:.2}%（{ok}/{data_rows}） ======",
        start.elapsed(),
        ok as f64 / data_rows as f64 * 100.0
    );
}

// ===== 纯 Rust md5/sha256（仅测试数据生成用） =====

fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6,
        10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in msg.chunks(64) {
        let mut m = [0u32; 16];
        for (i, w) in chunk.chunks(4).enumerate() {
            m[i] = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = String::with_capacity(32);
    for v in [a0, b0, c0, d0] {
        for b in v.to_le_bytes() {
            out.push_str(&format!("{b:02x}"));
        }
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = String::with_capacity(64);
    for v in h {
        out.push_str(&format!("{v:08x}"));
    }
    out
}
