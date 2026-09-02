//! gRPC 远程解码客户端（md5 / sha256 摘要查表）。
//!
//! 服务：encodemapping.EncodeMapping.query（proto 提取自 grpc-encode-mapping-rely-1.0.0.jar）
//! 地址：grpc.brapp.com:443，HTTP/2 明文（与 Java 侧 usePlaintext 一致）
//! 协议：EncodeRequest{param=JSON 字符串} → ResultBean{status, code, message, data}
//! JSON 字段名保持 Java 侧拼写（含 "alogrithm" 这个服务端既有的拼写，勿改）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;

tonic::include_proto!("encodemapping");

use self::encode_mapping_client::EncodeMappingClient;

/// 内置默认连接配置（已对生产服务实测可用）
/// 注意：不带端口后缀 —— 显式 :443 会被 LB 拒绝（Connection refused）
pub const DEFAULT_TARGET: &str = "http://grpc.brapp.com";
pub const DEFAULT_APP_NAME: &str = "marketing";
pub const DEFAULT_APP_SECRET: &str = "c6acc38a39b0769fa7fb1a95f82d9b33";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GrpcConfig {
    pub target: String,
    pub app_name: String,
    pub app_secret: String,
    /// 并发查询数
    pub concurrency: usize,
    /// 单次查询超时（毫秒）
    pub timeout_ms: u64,
}

impl Default for GrpcConfig {
    fn default() -> Self {
        Self {
            target: DEFAULT_TARGET.to_string(),
            app_name: DEFAULT_APP_NAME.to_string(),
            app_secret: DEFAULT_APP_SECRET.to_string(),
            concurrency: 200,
            timeout_ms: 8000,
        }
    }
}

pub struct DecodeClient {
    config: GrpcConfig,
    client: EncodeMappingClient<Channel>,
}

impl DecodeClient {
    pub async fn connect(config: GrpcConfig) -> Result<Self, String> {
        let endpoint = tonic::transport::Endpoint::from_shared(config.target.clone())
            .map_err(|e| format!("无效的服务地址: {e}"))?
            .connect_timeout(Duration::from_secs(10))
            .tcp_nodelay(true);
        let channel = endpoint
            .connect()
            .await
            .map_err(|e| format!("无法连接加解密服务: {e}"))?;
        Ok(Self {
            client: EncodeMappingClient::new(channel),
            config,
        })
    }

    /// 连通性探测（服务端要求集群外客户端必须先 ping）
    pub async fn ping(&mut self) -> Result<String, String> {
        let req = PingRequest {
            param: "desktop-ping".to_string(),
        };
        let mut cli = self.client.clone();
        let fut = cli.ping(req);
        let resp = tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .map_err(|_| "ping 超时".to_string())?
            .map_err(|e| format!("ping 失败: {e}"))?;
        Ok(resp.into_inner().response)
    }

    fn build_param(&self, key: &str, algo: &str) -> String {
        serde_json::json!({
            "swift_number": uuid::Uuid::new_v4().to_string(),
            "appName": self.config.app_name,
            "appSecretKey": self.config.app_secret,
            "key": key,
            "alogrithm": algo,
            "type": "cell",
        })
        .to_string()
    }

    /// 查询单个摘要。Ok(None) 表示服务端查无此映射。
    pub async fn query_one(&self, key: &str, algo: &str) -> Result<Option<String>, String> {
        let req = EncodeRequest {
            param: self.build_param(key, algo),
        };
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut cli = self.client.clone();
            let fut = cli.query(req.clone());
            match tokio::time::timeout(Duration::from_millis(self.config.timeout_ms), fut).await {
                Ok(Ok(resp)) => {
                    let bean = resp.into_inner();
                    let data = bean.data.trim().to_string();
                    if !data.is_empty() {
                        return Ok(Some(data));
                    }
                    let status = bean.status.trim().to_string();
                    if !status.is_empty() && status != "0" {
                        return Err(format!(
                            "服务返回错误 status={status} message={}",
                            bean.message
                        ));
                    }
                    return Ok(None);
                }
                Ok(Err(status)) => {
                    // 传输层错误重试一次（鉴权类错误重试同样失败，无副作用）
                    if attempt >= 2 {
                        return Err(format!("查询失败: {status}"));
                    }
                }
                Err(_) => {
                    if attempt >= 2 {
                        return Err("查询超时".to_string());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// 批量解码（去重后的键集合），带并发控制、进度回调、取消支持。
    /// 返回 (key -> Option<明文>, key -> 错误信息)；单键失败不中断整体。
    pub async fn decode_batch<F>(
        &self,
        keys: Vec<String>,
        algo: &'static str,
        cancel: &CancellationToken,
        mut on_progress: F,
    ) -> (HashMap<String, Option<String>>, HashMap<String, String>)
    where
        F: FnMut(usize, usize),
    {
        let total = keys.len();
        let results: Arc<Mutex<HashMap<String, Option<String>>>> =
            Arc::new(Mutex::new(HashMap::with_capacity(total)));
        let errors: Arc<Mutex<HashMap<String, String>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let mut stream = futures::stream::iter(keys.into_iter().map(|key| {
            let results = Arc::clone(&results);
            let errors = Arc::clone(&errors);
            async move {
                match self.query_one(&key, algo).await {
                    Ok(v) => {
                        results.lock().unwrap().insert(key, v);
                    }
                    Err(e) => {
                        errors.lock().unwrap().insert(key, e);
                    }
                }
            }
        }))
        .buffer_unordered(self.config.concurrency.max(1));

        let mut done = 0usize;
        while stream.next().await.is_some() {
            done += 1;
            if done % 200 == 0 || done == total {
                on_progress(done, total);
            }
            if cancel.is_cancelled() {
                break;
            }
        }
        drop(stream);

        let results = Arc::try_unwrap(results)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default();
        let errors = Arc::try_unwrap(errors)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default();
        (results, errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 集成测试：真实内网 gRPC 服务（需要 VPN 内网环境）。
    /// 无网络环境下会失败，故标记 ignore，手动执行：
    ///   cargo test --lib -- --ignored
    #[tokio::test]
    #[ignore]
    async fn live_ping_and_query() {
        let mut client = DecodeClient::connect(GrpcConfig::default())
            .await
            .expect("连接失败（确认是否在内网/VPN 环境）");
        let pong = client.ping().await.expect("ping 失败");
        assert!(pong.starts_with("pong"), "unexpected pong: {pong}");

        // 已知映射：15023709720 的 md5（先前用 Java 客户端验证过）
        let md5_of_15023709720 = "5e5ca4a768d0556a7bd8b6b0f4894fe4";
        let v = client
            .query_one(md5_of_15023709720, "md5")
            .await
            .expect("查询失败");
        assert_eq!(v.as_deref(), Some("15023709720"));

        // 不存在的 key → Ok(None)
        let v = client
            .query_one("00000000000000000000000000000000", "md5")
            .await
            .expect("查询失败");
        assert!(v.is_none());
    }
}



