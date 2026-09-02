/** 与 Rust 侧结构体一一对应 */

export type CipherKind = 'log' | 'md5' | 'sha256' | 'plain_phone' | 'empty' | 'unknown'

export interface ColumnStat {
  index: number
  header: string
  log_count: number
  md5_count: number
  sha256_count: number
  plain_count: number
  empty_count: number
  unknown_count: number
  sampled: number
}

export interface ImportPreview {
  file_name: string
  file_type: 'xlsx' | 'csv'
  sheets: string[]
  sheet: string
  headers: string[]
  sample_rows: string[][]
  column_stats: ColumnStat[]
  suggested_column: number
  total_rows: number
}

export interface PreviewItem {
  row_no: number
  original: string
  kind: string
  result: string
  ok: boolean
}

export interface GrpcConfig {
  target: string
  app_name: string
  app_secret: string
  concurrency: number
  timeout_ms: number
}

export type DecodeMode = 'auto' | 'log' | 'md5' | 'sha256'

export interface ExecuteRequest {
  path: string
  sheet: string
  column: number
  mode: DecodeMode
  grpc: GrpcConfig
  output_column_name: string
}

export interface FailedRow {
  row_no: number
  value: string
  reason: string
}

export interface ExecuteStats {
  total_rows: number
  success: number
  invalid_format: number
  plaintext: number
  empty: number
  not_found: number
  failed: number
  grpc_error_rows: number
  unique_remote_keys: number
  output_path: string
  failures_path: string | null
  failed_rows: FailedRow[]
  duration_ms: number
  cancelled: boolean
}

export interface ProgressPayload {
  phase: 'reading' | 'analyzing' | 'decoding' | 'writing' | 'done'
  processed: number
  total: number
  message: string
}

export const DEFAULT_GRPC_CONFIG: GrpcConfig = {
  target: 'http://grpc.brapp.com',
  app_name: 'marketing',
  app_secret: 'c6acc38a39b0769fa7fb1a95f82d9b33',
  concurrency: 200,
  timeout_ms: 8000,
}
