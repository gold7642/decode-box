//! 文件处理流水线：xlsx/csv 读取 → 密文识别与解密 → 追加解密列 → 写出。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use calamine::{Data, Range, Reader, Xlsx};
use encoding_rs::{UTF_16BE, UTF_16LE, GB18030};
use tauri::Emitter;
use tokio_util::sync::CancellationToken;

use crate::cipher::{self, LogDecodeOutcome};
use crate::detect::{self, CipherKind};
use crate::grpc_client::{DecodeClient, GrpcConfig};

const PROGRESS_EVENT: &str = "decrypt://progress";
const STATS_SAMPLE_ROWS: usize = 1000;
const PREVIEW_ROWS: usize = 5;
const FAILED_LIST_IN_MEMORY_CAP: usize = 10_000;
const DEFAULT_OUTPUT_COLUMN: &str = "解密结果";

// ===================== 数据结构 =====================

#[derive(Debug, Clone, serde::Serialize)]
pub struct ColumnStat {
    pub index: usize,
    pub header: String,
    pub log_count: usize,
    pub md5_count: usize,
    pub sha256_count: usize,
    pub plain_count: usize,
    pub empty_count: usize,
    pub unknown_count: usize,
    pub sampled: usize,
}

impl ColumnStat {
    fn cipher_hits(&self) -> usize {
        self.log_count + self.md5_count + self.sha256_count
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportPreview {
    pub file_name: String,
    pub file_type: String,
    pub sheets: Vec<String>,
    pub sheet: String,
    pub headers: Vec<String>,
    pub sample_rows: Vec<Vec<String>>,
    pub column_stats: Vec<ColumnStat>,
    pub suggested_column: usize,
    pub total_rows: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PreviewItem {
    pub row_no: usize,
    pub original: String,
    pub kind: String,
    pub result: String,
    pub ok: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ExecuteRequest {
    pub path: String,
    pub sheet: String,
    pub column: usize,
    /// auto | log | md5 | sha256
    pub mode: String,
    #[serde(default)]
    pub grpc: GrpcConfig,
    #[serde(default = "default_output_column")]
    pub output_column_name: String,
}

fn default_output_column() -> String {
    DEFAULT_OUTPUT_COLUMN.to_string()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FailedRow {
    pub row_no: usize,
    pub value: String,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ExecuteStats {
    pub total_rows: usize,
    pub success: usize,
    pub invalid_format: usize,
    pub plaintext: usize,
    pub empty: usize,
    pub not_found: usize,
    pub failed: usize,
    pub grpc_error_rows: usize,
    pub unique_remote_keys: usize,
    pub output_path: String,
    pub failures_path: Option<String>,
    pub failed_rows: Vec<FailedRow>,
    pub duration_ms: u64,
    pub cancelled: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ProgressPayload {
    phase: String,
    processed: usize,
    total: usize,
    message: String,
}

struct FileData {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    file_type: String,
    sheet_name: String,
    all_sheets: Vec<String>,
    csv_delimiter: u8,
}

/// 每个数据行的最终分类（写出与统计共用，保证口径一致）
#[derive(Debug, Clone)]
struct FinalRow {
    /// 写入解密列的值（Plain 透传原值；失败留空）
    value: String,
    /// 是否"正常"（决定 xlsx 是否标红）
    ok: bool,
    kind: FinalKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalKind {
    Success,
    InvalidFormat,
    Plain,
    Empty,
    NotFound,
    GrpcError,
    /// 具体失败原因（如"无法识别的密文"、"log 解密失败"）
    Failed(&'static str),
}

// ===================== 读取 =====================

fn cell_to_string(c: &Data) -> String {
    match c {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Float(f) => {
            // 整数值去掉 .0，避免 11 位手机号被写成 "13800138000.0"
            if f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 {
                format!("{}", *f as i64)
            } else {
                f.to_string()
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn read_xlsx(path: &Path, sheet: Option<&str>) -> Result<FileData, String> {
    let mut workbook: Xlsx<_> =
        calamine::open_workbook(path).map_err(|e| format!("打开 xlsx 失败: {e}"))?;
    let all_sheets = workbook.sheet_names().to_vec();
    if all_sheets.is_empty() {
        return Err("工作簿中没有工作表".to_string());
    }
    let sheet_name = match sheet {
        Some(s) if !s.is_empty() => {
            if !all_sheets.iter().any(|x| x == s) {
                return Err(format!("工作表 {s} 不存在"));
            }
            s.to_string()
        }
        _ => all_sheets[0].clone(),
    };
    let range: Range<Data> = workbook
        .worksheet_range(&sheet_name)
        .map_err(|e| format!("读取工作表 {sheet_name} 失败: {e}"))?;
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(range.height());
    for row in range.rows() {
        rows.push(row.iter().map(cell_to_string).collect());
    }
    normalize(&mut rows);
    let headers = rows
        .first()
        .cloned()
        .ok_or_else(|| "工作表为空".to_string())?;
    Ok(FileData {
        headers,
        rows,
        file_type: "xlsx".into(),
        sheet_name,
        all_sheets,
        csv_delimiter: b',',
    })
}

fn decode_csv_bytes(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return UTF_16LE.decode(&bytes[2..]).0.into_owned();
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return UTF_16BE.decode(&bytes[2..]).0.into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    // 国内 Excel 导出的 CSV 常见 GBK/GB18030
    GB18030.decode(bytes).0.into_owned()
}

fn sniff_delimiter(text: &str) -> u8 {
    let first_line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    let (mut comma, mut tab, mut semi) = (0usize, 0usize, 0usize);
    let mut in_quotes = false;
    for c in first_line.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => comma += 1,
            '\t' if !in_quotes => tab += 1,
            ';' if !in_quotes => semi += 1,
            _ => {}
        }
    }
    if tab > comma && tab > semi {
        b'\t'
    } else if semi > comma {
        b';'
    } else {
        b','
    }
}

fn read_csv(path: &Path) -> Result<FileData, String> {
    let bytes = fs::read(path).map_err(|e| format!("读取文件失败: {e}"))?;
    let text = decode_csv_bytes(&bytes);
    let delimiter = sniff_delimiter(&text);
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .has_headers(false)
        .from_reader(text.as_bytes());
    let mut rows: Vec<Vec<String>> = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| format!("解析 CSV 第 {} 行失败: {e}", rows.len() + 1))?;
        rows.push(rec.iter().map(|s| s.to_string()).collect());
    }
    normalize(&mut rows);
    let headers = rows
        .first()
        .cloned()
        .ok_or_else(|| "CSV 文件为空".to_string())?;
    Ok(FileData {
        headers,
        rows,
        file_type: "csv".into(),
        sheet_name: String::new(),
        all_sheets: vec![],
        csv_delimiter: delimiter,
    })
}

/// 行宽归一：补齐到最大宽度（保证解密列插入位置一致）
fn normalize(rows: &mut Vec<Vec<String>>) {
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    for r in rows.iter_mut() {
        r.resize(width, String::new());
    }
}

fn read_file(path: &str, sheet: Option<&str>) -> Result<FileData, String> {
    let p = Path::new(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "xlsx" => read_xlsx(p, sheet),
        "csv" | "txt" => read_csv(p),
        "xls" => Err("暂不支持老版 .xls，请先在 Excel 中另存为 .xlsx".to_string()),
        other => Err(format!("不支持的文件类型 .{other}，仅支持 .xlsx / .csv")),
    }
}

// ===================== 导入预览 =====================

fn build_preview(data: &FileData, path: &str) -> ImportPreview {
    let sample_rows: Vec<Vec<String>> = data.rows.iter().skip(1).take(8).cloned().collect();
    let mut column_stats = Vec::with_capacity(data.headers.len());
    for (idx, header) in data.headers.iter().enumerate() {
        let mut stat = ColumnStat {
            index: idx,
            header: header.clone(),
            log_count: 0,
            md5_count: 0,
            sha256_count: 0,
            plain_count: 0,
            empty_count: 0,
            unknown_count: 0,
            sampled: 0,
        };
        for row in data.rows.iter().skip(1).take(STATS_SAMPLE_ROWS) {
            let v = row.get(idx).map(|s| s.as_str()).unwrap_or("");
            match detect::detect(v) {
                CipherKind::Log => stat.log_count += 1,
                CipherKind::Md5 => stat.md5_count += 1,
                CipherKind::Sha256 => stat.sha256_count += 1,
                CipherKind::PlainPhone => stat.plain_count += 1,
                CipherKind::Empty => stat.empty_count += 1,
                CipherKind::Unknown => stat.unknown_count += 1,
            }
            stat.sampled += 1;
        }
        column_stats.push(stat);
    }
    // 建议列：密文命中最多者；列名含手机号关键词加分
    let mut suggested = 0usize;
    let mut best_score = -1i64;
    for st in &column_stats {
        let name_hit = ["phone", "cell", "mobile", "手机", "电话"]
            .iter()
            .any(|k| st.header.to_lowercase().contains(k));
        let score = st.cipher_hits() as i64 + i64::from(name_hit);
        if score > best_score {
            best_score = score;
            suggested = st.index;
        }
    }
    ImportPreview {
        file_name: Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        file_type: data.file_type.clone(),
        sheets: data.all_sheets.clone(),
        sheet: data.sheet_name.clone(),
        headers: data.headers.clone(),
        sample_rows,
        column_stats,
        suggested_column: suggested,
        total_rows: data.rows.len().saturating_sub(1),
    }
}

pub async fn preview(path: String, sheet: Option<String>) -> Result<ImportPreview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let data = read_file(&path, sheet.as_deref())?;
        Ok(build_preview(&data, &path))
    })
    .await
    .map_err(|e| format!("读取任务失败: {e}"))?
}

// ===================== 试解预览 =====================

fn classify_for_mode(value: &str, mode: &str) -> (CipherKind, String) {
    let v = value.trim();
    match mode {
        "log" => (CipherKind::Log, v.to_string()),
        "md5" => (CipherKind::Md5, v.to_string()),
        "sha256" => (CipherKind::Sha256, v.to_string()),
        _ => (detect::detect(v), v.to_string()),
    }
}

pub async fn preview_decrypt(
    path: String,
    sheet: Option<String>,
    column: usize,
    mode: String,
    grpc: GrpcConfig,
) -> Result<Vec<PreviewItem>, String> {
    let data = read_file(&path, sheet.as_deref())?;
    if column >= data.headers.len() {
        return Err("加密列不存在".to_string());
    }
    let mut items: Vec<PreviewItem> = Vec::new();
    let mut remote: Vec<(usize, String, CipherKind)> = Vec::new();
    for (i, row) in data.rows.iter().enumerate().skip(1) {
        if items.len() + remote.len() >= PREVIEW_ROWS {
            break;
        }
        let raw = row.get(column).cloned().unwrap_or_default();
        let (kind, v) = classify_for_mode(&raw, &mode);
        let row_no = i + 2; // 含表头的 1-based 行号
        match kind {
            CipherKind::Log => {
                let (result, ok) = match cipher::decode(&v) {
                    LogDecodeOutcome::Decoded(s) => (s, true),
                    LogDecodeOutcome::NotCipher => ("非 log 密文".into(), false),
                    LogDecodeOutcome::Failed => ("解密失败".into(), false),
                };
                items.push(PreviewItem {
                    row_no,
                    original: raw,
                    kind: kind.label().to_string(),
                    result,
                    ok,
                });
            }
            CipherKind::Md5 | CipherKind::Sha256 => remote.push((row_no, v, kind)),
            CipherKind::PlainPhone => items.push(PreviewItem {
                row_no,
                original: raw,
                kind: kind.label().to_string(),
                result: v,
                ok: true,
            }),
            CipherKind::Empty => items.push(PreviewItem {
                row_no,
                original: raw,
                kind: kind.label().to_string(),
                result: String::new(),
                ok: true,
            }),
            CipherKind::Unknown => items.push(PreviewItem {
                row_no,
                original: raw,
                kind: kind.label().to_string(),
                result: "无法识别的密文".into(),
                ok: false,
            }),
        }
    }
    if !remote.is_empty() {
        let client = DecodeClient::connect(grpc).await?;
        for (row_no, key, kind) in remote {
            let algo: &'static str = if kind == CipherKind::Sha256 {
                "sha"
            } else {
                "md5"
            };
            let result = match client.query_one(&key, algo).await {
                Ok(Some(v)) => v,
                Ok(None) => "服务端查无映射".into(),
                Err(e) => format!("查询失败: {e}"),
            };
            let ok = detect::is_mainland_phone(&result);
            items.push(PreviewItem {
                row_no,
                original: key,
                kind: kind.label().to_string(),
                result,
                ok,
            });
        }
    }
    Ok(items)
}

// ===================== 执行 =====================

enum RowOutcome {
    /// 已本地解出（值, 是否合法手机号）
    Value(String, bool),
    Plain,
    Empty,
    Failed(&'static str),
    /// 待远程查询（key）
    Remote(String),
}

fn emit_progress(app: &tauri::AppHandle, phase: &str, processed: usize, total: usize, msg: &str) {
    let _ = app.emit(
        PROGRESS_EVENT,
        ProgressPayload {
            phase: phase.to_string(),
            processed,
            total,
            message: msg.to_string(),
        },
    );
}

fn unique_output_path(input: &str) -> PathBuf {
    let p = Path::new(input);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = p.parent().unwrap_or_else(|| Path::new("."));
    let mut candidate = dir.join(format!("{stem}_decrypted.{ext}"));
    let mut n = 0u32;
    while candidate.exists() {
        n += 1;
        candidate = dir.join(format!("{stem}_decrypted_{n}.{ext}"));
    }
    candidate
}

pub async fn execute(
    app: tauri::AppHandle,
    req: ExecuteRequest,
    cancel: CancellationToken,
) -> Result<ExecuteStats, String> {
    let start = Instant::now();
    emit_progress(&app, "reading", 0, 0, "读取文件…");
    let (path, sheet) = (req.path.clone(), req.sheet.clone());
    let data = tauri::async_runtime::spawn_blocking(move || read_file(&path, Some(&sheet)))
        .await
        .map_err(|e| format!("读取任务失败: {e}"))??;

    if cancel.is_cancelled() {
        return Ok(cancelled_stats(start));
    }
    if req.column >= data.headers.len() {
        return Err("加密列不存在".to_string());
    }

    // ===== 分类 =====
    emit_progress(&app, "analyzing", 0, data.rows.len(), "分析密文类型…");
    let mut outcomes: Vec<RowOutcome> = Vec::with_capacity(data.rows.len() - 1);
    let mut remote_keys: HashMap<CipherKind, HashSet<String>> = HashMap::new();
    for row in data.rows.iter().skip(1) {
        let raw = row.get(req.column).cloned().unwrap_or_default();
        let (kind, v) = classify_for_mode(&raw, &req.mode);
        outcomes.push(match kind {
            CipherKind::Log => match cipher::decode(&v) {
                LogDecodeOutcome::Decoded(s) => {
                    RowOutcome::Value(s.clone(), detect::is_mainland_phone(&s))
                }
                LogDecodeOutcome::NotCipher => RowOutcome::Failed("非 log 密文"),
                LogDecodeOutcome::Failed => RowOutcome::Failed("log 解密失败"),
            },
            CipherKind::Md5 | CipherKind::Sha256 => {
                remote_keys.entry(kind).or_default().insert(v.clone());
                RowOutcome::Remote(v)
            }
            CipherKind::PlainPhone => RowOutcome::Plain,
            CipherKind::Empty => RowOutcome::Empty,
            CipherKind::Unknown => RowOutcome::Failed("无法识别的密文"),
        });
    }

    // ===== 远程解码 =====
    let mut remote_map: HashMap<String, Option<String>> = HashMap::new();
    let mut remote_errs: HashMap<String, String> = HashMap::new();
    let unique_remote_keys: usize = remote_keys.values().map(|s| s.len()).sum();
    if unique_remote_keys > 0 {
        if cancel.is_cancelled() {
            return Ok(cancelled_stats(start));
        }
        emit_progress(
            &app,
            "decoding",
            0,
            unique_remote_keys,
            &format!("远程查表解密（去重后 {unique_remote_keys} 条）…"),
        );
        let mut client = DecodeClient::connect(req.grpc.clone()).await?;
        let _ = client.ping().await; // 连通性预热（服务端要求集群外客户端先 ping）
        for (kind, keys) in remote_keys {
            if cancel.is_cancelled() {
                return Ok(cancelled_stats(start));
            }
            let algo: &'static str = if kind == CipherKind::Sha256 {
                "sha"
            } else {
                "md5"
            };
            let key_list: Vec<String> = keys.into_iter().collect();
            let app2 = app.clone();
            let (res, errs) = client
                .decode_batch(key_list, algo, &cancel, move |done, t| {
                    emit_progress(&app2, "decoding", done, t, "远程查表解密…");
                })
                .await;
            remote_map.extend(res);
            remote_errs.extend(errs);
        }
    }

    if cancel.is_cancelled() {
        return Ok(cancelled_stats(start));
    }

    // ===== 汇总每行最终结果（写出与统计共用同一口径） =====
    let mut finals: Vec<FinalRow> = Vec::with_capacity(outcomes.len());
    for (i, o) in outcomes.iter().enumerate() {
        let orig = data
            .rows
            .get(i + 1)
            .and_then(|r| r.get(req.column))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        finals.push(match o {
            RowOutcome::Value(v, valid) => FinalRow {
                value: v.clone(),
                ok: *valid,
                kind: if *valid {
                    FinalKind::Success
                } else {
                    FinalKind::InvalidFormat
                },
            },
            RowOutcome::Plain => FinalRow {
                value: orig,
                ok: true,
                kind: FinalKind::Plain,
            },
            RowOutcome::Empty => FinalRow {
                value: String::new(),
                ok: true,
                kind: FinalKind::Empty,
            },
            RowOutcome::Failed(reason) => FinalRow {
                value: String::new(),
                ok: false,
                kind: FinalKind::Failed(reason),
            },
            RowOutcome::Remote(key) => match remote_map.get(key) {
                Some(Some(phone)) => FinalRow {
                    value: phone.clone(),
                    ok: detect::is_mainland_phone(phone),
                    kind: if detect::is_mainland_phone(phone) {
                        FinalKind::Success
                    } else {
                        FinalKind::InvalidFormat
                    },
                },
                Some(None) => FinalRow {
                    value: String::new(),
                    ok: false,
                    kind: FinalKind::NotFound,
                },
                None => FinalRow {
                    value: String::new(),
                    ok: false,
                    kind: FinalKind::GrpcError,
                },
            },
        });
    }

    // ===== 统计与失败清单（先于写出：finals 只 borrow） =====
    let column = req.column;
    let mut failed_indices: Vec<usize> = Vec::new();
    let mut stats = ExecuteStats {
        total_rows: finals.len(),
        success: 0,
        invalid_format: 0,
        plaintext: 0,
        empty: 0,
        not_found: 0,
        failed: 0,
        grpc_error_rows: 0,
        unique_remote_keys,
        output_path: String::new(),
        failures_path: None,
        failed_rows: Vec::new(),
        duration_ms: 0,
        cancelled: false,
    };
    for (i, f) in finals.iter().enumerate() {
        let reason = match f.kind {
            FinalKind::InvalidFormat => Some("解密结果不是合法手机号"),
            FinalKind::NotFound => Some("服务端查无映射"),
            FinalKind::GrpcError => Some("远程查询失败"),
            FinalKind::Failed(r) => Some(r),
            _ => None,
        };
        if let Some(r) = reason {
            failed_indices.push(i);
            if stats.failed_rows.len() < FAILED_LIST_IN_MEMORY_CAP {
                let raw = data
                    .rows
                    .get(i + 1)
                    .and_then(|row| row.get(column))
                    .cloned()
                    .unwrap_or_default();
                stats.failed_rows.push(FailedRow {
                    row_no: i + 2,
                    value: raw,
                    reason: r.to_string(),
                });
            }
        }
        match f.kind {
            FinalKind::Success => stats.success += 1,
            FinalKind::InvalidFormat => stats.invalid_format += 1,
            FinalKind::Plain => stats.plaintext += 1,
            FinalKind::Empty => stats.empty += 1,
            FinalKind::NotFound => stats.not_found += 1,
            FinalKind::GrpcError => stats.grpc_error_rows += 1,
            FinalKind::Failed(_) => stats.failed += 1,
        }
    }

    // ===== 写出 =====
    let out_path = unique_output_path(&req.path);
    let out_path_str = out_path.to_string_lossy().into_owned();
    let out_path_for_cleanup = out_path.clone();
    emit_progress(&app, "writing", 0, finals.len(), "写出文件…");
    let write_app = app.clone();
    let write_cancel = cancel.clone();
    let headers = data.headers.clone();
    let rows = data.rows.clone();
    let file_type = data.file_type.clone();
    let sheet_name = data.sheet_name.clone();
    let delimiter = data.csv_delimiter;
    let output_column_name = if req.output_column_name.trim().is_empty() {
        DEFAULT_OUTPUT_COLUMN.to_string()
    } else {
        req.output_column_name.trim().to_string()
    };
    let write_result = tauri::async_runtime::spawn_blocking(move || {
        write_output(
            &write_app,
            &write_cancel,
            &headers,
            &rows,
            column,
            &output_column_name,
            &finals,
            &out_path,
            &file_type,
            &sheet_name,
            delimiter,
        )
    })
    .await
    .map_err(|e| format!("写出任务失败: {e}"))?;

    if cancel.is_cancelled() {
        let _ = fs::remove_file(&out_path_for_cleanup);
        return Ok(cancelled_stats(start));
    }
    write_result?;

    stats.output_path = out_path_str.clone();
    if !failed_indices.is_empty() {
        // 失败数据单独落文件：字段与原文一致（原表头 + 失败行的原始完整行）
        let failed_rows_data: Vec<Vec<String>> = failed_indices
            .iter()
            .filter_map(|&i| data.rows.get(i + 1).cloned())
            .collect();
        let fail_path = write_failures_file(
            Path::new(&out_path_str),
            &data.headers,
            &failed_rows_data,
            &data.file_type,
            &data.sheet_name,
            data.csv_delimiter,
        );
        stats.failures_path = Some(fail_path);
    }
    stats.duration_ms = start.elapsed().as_millis() as u64;
    emit_progress(&app, "done", 1, 1, "完成");
    Ok(stats)
}

fn cancelled_stats(start: Instant) -> ExecuteStats {
    ExecuteStats {
        total_rows: 0,
        success: 0,
        invalid_format: 0,
        plaintext: 0,
        empty: 0,
        not_found: 0,
        failed: 0,
        grpc_error_rows: 0,
        unique_remote_keys: 0,
        output_path: String::new(),
        failures_path: None,
        failed_rows: Vec::new(),
        duration_ms: start.elapsed().as_millis() as u64,
        cancelled: true,
    }
}

#[allow(clippy::too_many_arguments)]
fn write_output(
    app: &tauri::AppHandle,
    cancel: &CancellationToken,
    headers: &[String],
    rows: &[Vec<String>],
    column: usize,
    output_column_name: &str,
    finals: &[FinalRow],
    out_path: &Path,
    file_type: &str,
    sheet_name: &str,
    delimiter: u8,
) -> Result<(), String> {
    match file_type {
        "csv" => write_csv_output(
            app, cancel, headers, rows, column, output_column_name, finals, out_path, delimiter,
        ),
        _ => write_xlsx_output(
            app, cancel, headers, rows, column, output_column_name, finals, out_path, sheet_name,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_csv_output(
    app: &tauri::AppHandle,
    cancel: &CancellationToken,
    headers: &[String],
    rows: &[Vec<String>],
    column: usize,
    output_column_name: &str,
    finals: &[FinalRow],
    out_path: &Path,
    delimiter: u8,
) -> Result<(), String> {
    let mut file = fs::File::create(out_path).map_err(|e| format!("创建输出文件失败: {e}"))?;
    // UTF-8 BOM：保证 Excel 直接打开不乱码
    file.write_all(&[0xEF, 0xBB, 0xBF])
        .map_err(|e| e.to_string())?;
    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(file);

    let mut header_row: Vec<String> = headers.to_vec();
    header_row.insert(column + 1, output_column_name.to_string());
    wtr.write_record(&header_row).map_err(|e| e.to_string())?;

    for (i, row) in rows.iter().enumerate().skip(1) {
        if cancel.is_cancelled() {
            return Err("已取消".into());
        }
        if i % 5000 == 0 {
            emit_progress(app, "writing", i - 1, finals.len(), "写出文件…");
        }
        let val = finals.get(i - 1).map(|f| f.value.as_str()).unwrap_or("");
        let mut out: Vec<String> = row.clone();
        out.insert(column + 1, val.to_string());
        wtr.write_record(&out).map_err(|e| e.to_string())?;
    }
    wtr.flush().map_err(|e| e.to_string())?;
    emit_progress(app, "writing", finals.len(), finals.len(), "写出完成");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_xlsx_output(
    app: &tauri::AppHandle,
    cancel: &CancellationToken,
    headers: &[String],
    rows: &[Vec<String>],
    column: usize,
    output_column_name: &str,
    finals: &[FinalRow],
    out_path: &Path,
    sheet_name: &str,
) -> Result<(), String> {
    use rust_xlsxwriter::{Format, Workbook};

    // 常量内存模式：顺序写行，内存占用与行数无关
    let mut workbook = Workbook::new();
    let worksheet = workbook
        .add_worksheet_with_constant_memory()
        .set_name(&sanitize_sheet_name(sheet_name))
        .map_err(|e| format!("设置工作表名失败: {e}"))?;
    let header_fmt = Format::new().set_bold();
    let fail_fmt = Format::new().set_font_color("#C0392B");

    // 表头（在加密列右侧插入解密列）
    let mut col_idx = 0u16;
    for (c, h) in headers.iter().enumerate() {
        if c == column + 1 {
            worksheet
                .write_string_with_format(0, col_idx, output_column_name, &header_fmt)
                .map_err(|e| e.to_string())?;
            col_idx += 1;
        }
        worksheet
            .write_string_with_format(0, col_idx, h, &header_fmt)
            .map_err(|e| e.to_string())?;
        col_idx += 1;
    }
    if column + 1 >= headers.len() {
        worksheet
            .write_string_with_format(0, col_idx, output_column_name, &header_fmt)
            .map_err(|e| e.to_string())?;
    }

    // 数据行（0-based：i=1 对应 Excel 第 2 行）
    for (i, row) in rows.iter().enumerate().skip(1) {
        if cancel.is_cancelled() {
            return Err("已取消".into());
        }
        if i % 5000 == 0 {
            emit_progress(app, "writing", i - 1, finals.len(), "写出文件…");
        }
        let excel_row = i as u32;
        let (val, ok) = finals
            .get(i - 1)
            .map(|f| (f.value.as_str(), f.ok))
            .unwrap_or(("", true));
        let write_decoded = |ws: &mut rust_xlsxwriter::Worksheet, col: u16| -> Result<(), String> {
            if ok {
                ws.write_string(excel_row, col, val)
                    .map_err(|e| e.to_string())?;
            } else {
                ws.write_string_with_format(excel_row, col, val, &fail_fmt)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        };
        let mut col_idx = 0u16;
        for (c, cell) in row.iter().enumerate() {
            if c == column + 1 {
                write_decoded(worksheet, col_idx)?;
                col_idx += 1;
            }
            worksheet
                .write_string(excel_row, col_idx, cell)
                .map_err(|e| e.to_string())?;
            col_idx += 1;
        }
        if column + 1 >= row.len() {
            write_decoded(worksheet, col_idx)?;
        }
    }
    workbook
        .save(out_path)
        .map_err(|e| format!("保存失败: {e}"))?;
    emit_progress(app, "writing", finals.len(), finals.len(), "写出完成");
    Ok(())
}

fn sanitize_sheet_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '[' | ']' | ':' | '*' | '?' | '/' | '\\' => '_',
            _ => c,
        })
        .collect();
    if s.is_empty() {
        s = "Sheet1".to_string();
    }
    if s.chars().count() > 31 {
        s = s.chars().take(31).collect();
    }
    s
}

/// 失败数据单独落文件：字段与原文一致（原表头 + 失败行的原始完整行，不追加任何列）。
/// 格式跟随原文（csv → csv，xlsx → xlsx）。
fn write_failures_file(
    out_path: &Path,
    headers: &[String],
    rows: &[Vec<String>],
    file_type: &str,
    sheet_name: &str,
    delimiter: u8,
) -> String {
    let stem = out_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = if file_type == "csv" { "csv" } else { "xlsx" };
    let fail_path = out_path.with_file_name(format!("{stem}_failures.{ext}"));

    if file_type == "csv" {
        if let Ok(mut file) = fs::File::create(&fail_path) {
            let _ = file.write_all(&[0xEF, 0xBB, 0xBF]);
            let mut wtr = csv::WriterBuilder::new()
                .delimiter(delimiter)
                .from_writer(file);
            let _ = wtr.write_record(headers);
            for row in rows {
                let _ = wtr.write_record(row);
            }
            let _ = wtr.flush();
        }
    } else {
        use rust_xlsxwriter::{Format, Workbook};
        let mut workbook = Workbook::new();
        if let Ok(worksheet) = workbook
            .add_worksheet()
            .set_name(&sanitize_sheet_name(sheet_name))
        {
            let header_fmt = Format::new().set_bold();
            for (c, h) in headers.iter().enumerate() {
                let _ = worksheet.write_string_with_format(0, c as u16, h, &header_fmt);
            }
            for (r, row) in rows.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    let _ = worksheet.write_string((r + 1) as u32, c as u16, cell);
                }
            }
        }
        let _ = workbook.save(&fail_path);
    }
    fail_path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_file_matches_original_schema_csv() {
        let dir = std::env::temp_dir().join("dctest_failures_csv");
        let _ = fs::create_dir_all(&dir);
        let out_path = dir.join("sample_decrypted.csv");
        let headers = vec!["phone_encrypted".to_string()];
        let rows = vec![
            vec!["Uw0JCΒ6goHAVhTV1Q".to_string()],
            vec!["invalid_data_0001".to_string()],
        ];
        let fail_path = write_failures_file(&out_path, &headers, &rows, "csv", "", b',');

        let mut bytes = fs::read(&fail_path).unwrap();
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            bytes.drain(..3);
        }
        let text = String::from_utf8(bytes).unwrap();
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(text.as_bytes());
        let records: Vec<Vec<String>> = rdr
            .records()
            .map(|r| r.unwrap().iter().map(|s| s.to_string()).collect())
            .collect();

        // 表头 + 2 行，字段与原文一致（单列 phone_encrypted）
        assert_eq!(records.len(), 3);
        assert_eq!(records[0], vec!["phone_encrypted".to_string()]);
        assert_eq!(records[1], vec!["Uw0JCΒ6goHAVhTV1Q".to_string()]);
        assert_eq!(records[2], vec!["invalid_data_0001".to_string()]);
        let _ = fs::remove_file(&fail_path);
    }

    #[test]
    fn failures_file_follows_original_format_xlsx() {
        let dir = std::env::temp_dir().join("dctest_failures_xlsx");
        let _ = fs::create_dir_all(&dir);
        let out_path = dir.join("sample_decrypted.xlsx");
        let headers = vec!["phone_encrypted".to_string()];
        let rows = vec![vec!["Uw0JCΒ6goHAVhTV1Q".to_string()]];
        let fail_path = write_failures_file(&out_path, &headers, &rows, "xlsx", "Sheet1", b',');
        assert!(fail_path.ends_with("_failures.xlsx"));
        assert!(Path::new(&fail_path).exists());
        let _ = fs::remove_file(&fail_path);
    }
}
