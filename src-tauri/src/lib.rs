//! 手机号解密工具 —— Tauri 命令层。
//!
//! 支持三种密文形态：
//! - log 密文（含 Β/Α 分隔符）：本地解密，毫秒级
//! - md5 / sha256 摘要：远程 gRPC 查表（grpc.brapp.com，内置配置）

pub mod cipher;
pub mod detect;
pub mod grpc_client;
pub mod pipeline;

use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct AppState {
    cancel: Mutex<Option<CancellationToken>>,
}

/// 导入文件：解析表头、前 8 行预览、每列密文类型统计（前 1000 行采样）
#[tauri::command]
async fn import_file(
    path: String,
    sheet: Option<String>,
) -> Result<pipeline::ImportPreview, String> {
    pipeline::preview(path, sheet).await
}

/// 试解前 5 条：正式执行前确认解密方式与连通性
#[tauri::command]
async fn preview_decrypt(
    path: String,
    sheet: Option<String>,
    column: usize,
    mode: String,
    grpc: grpc_client::GrpcConfig,
) -> Result<Vec<pipeline::PreviewItem>, String> {
    pipeline::preview_decrypt(path, sheet, column, mode, grpc).await
}

/// 测试 gRPC 服务连通性（服务端要求集群外客户端先 ping）
#[tauri::command]
async fn ping_grpc(config: grpc_client::GrpcConfig) -> Result<String, String> {
    grpc_client::DecodeClient::connect(config)
        .await?
        .ping()
        .await
}

/// 执行解密：进度通过 `decrypt://progress` 事件推送
#[tauri::command]
async fn execute_decrypt(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    req: pipeline::ExecuteRequest,
) -> Result<pipeline::ExecuteStats, String> {
    let token = CancellationToken::new();
    *state.cancel.lock().unwrap() = Some(token.clone());
    let result = pipeline::execute(app, req, token).await;
    *state.cancel.lock().unwrap() = None;
    result
}

/// 取消当前执行
#[tauri::command]
fn cancel_decrypt(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    if let Some(token) = state.cancel.lock().unwrap().as_ref() {
        token.cancel();
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            import_file,
            preview_decrypt,
            ping_grpc,
            execute_decrypt,
            cancel_decrypt
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
