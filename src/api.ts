import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type {
  ExecuteRequest,
  ExecuteStats,
  GrpcConfig,
  ImportPreview,
  PreviewItem,
  ProgressPayload,
} from './types'

export async function importFile(path: string, sheet?: string): Promise<ImportPreview> {
  return invoke('import_file', { path, sheet: sheet ?? null })
}

export async function previewDecrypt(
  path: string,
  sheet: string | undefined,
  column: number,
  mode: string,
  grpc: GrpcConfig,
): Promise<PreviewItem[]> {
  return invoke('preview_decrypt', { path, sheet: sheet || null, column, mode, grpc })
}

export async function pingGrpc(config: GrpcConfig): Promise<string> {
  return invoke('ping_grpc', { config })
}

export async function executeDecrypt(req: ExecuteRequest): Promise<ExecuteStats> {
  return invoke('execute_decrypt', { req })
}

export async function cancelDecrypt(): Promise<boolean> {
  return invoke('cancel_decrypt')
}

export function onProgress(handler: (p: ProgressPayload) => void): Promise<UnlistenFn> {
  return listen<ProgressPayload>('decrypt://progress', (e) => handler(e.payload))
}
