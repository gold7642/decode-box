# 解码宝匣 DecodeBox

跨平台桌面客户端（macOS / Windows），批量解密 Excel/CSV 文件中的加密手机号，在原始加密列右侧追加一列解密结果。

## 支持的解密方式

| 方式 | 形态 | 实现 |
|---|---|---|
| **log 密文** | 含希腊字母 `Β`/`Α` 的 Base64 变体 | 本地解密（`BrCipherMaker` 算法移植，毫秒级，无需网络） |
| **md5** | 32 位十六进制 | 远程 gRPC 查表（`grpc.brapp.com`） |
| **sha256** | 64 位十六进制 | 远程 gRPC 查表（`grpc.brapp.com`） |
| 明文 | 11 位手机号 | 原样透传 |

> **自动识别**：导入后逐条判断密文类型，log / md5 / sha256 / 明文混合的文件无需预先分类。

## 使用

1. 拖拽或选择 `.xlsx` / `.csv` 文件（单列文件，取第一列）
2. 选择解密方式（默认「自动识别」，可指定 log / md5 / sha256）
3. 可选「试解前 5 条」确认正确性
4. 点击「开始解密」，查看进度与统计

输出为新文件 `<原名>_decrypted.<ext>`，原文件绝不修改。有失败行时同目录生成 `<原名>_decrypted_failures.<ext>`（格式跟随原文：csv→csv、xlsx→xlsx），失败清单字段与原文一致。

## 重要说明

- **md5 / sha256 是单向哈希，无法本地还原**。解密依赖内网加解密服务（`grpc.brapp.com`），使用前需处于内网/VPN 环境。
- **性能**：log 解密纯本地，20 万行约 1 秒；md5/sha256 受远程服务 QPS 限制（实测 ~600-800 查询/秒），工具已做**去重 + 并发**优化，相同密文只查一次。
- **密钥内置**：log 解密的 10 把密钥与远程服务的 appSecretKey 已内置（对齐现有 Java 生态）。这等同于「拿到工具 = 具备解密能力」，请仅在受控范围内分发。
- **数据敏感**：解密结果含明文手机号，属个人信息，请妥善保管输出文件。

## 开发与发布

```bash
# 前置：Node 18+、Rust (rustup)
npm install
npm run tauri dev        # 开发模式
npm run tauri build      # 打包当前平台（mac 出 .app/.dmg）
```

**三平台构建（mac arm64 / mac x64 / Windows）走 GitHub Actions**：修改代码 → `git push` → 云上自动出全平台安装包；打 `git tag v0.X.0` 自动生成 Release。完整流程见 **[ITERATING.md](ITERATING.md)**（含 Git 个人账号隔离、发布方案、踩坑记录）。

仓库：https://github.com/gold7642/decode-box（私有）；正式下载在 **Releases** 页面。

## 项目结构

```
src-tauri/
  proto/encodemapping.proto   # 从 grpc-encode-mapping-rely jar 提取的协议
  src/
    cipher.rs                 # log 解密算法（BrCipherMaker 移植，含验证向量）
    detect.rs                 # 密文类型自动识别
    grpc_client.rs            # md5/sha256 远程查表客户端
    pipeline.rs               # 文件读取 → 解密 → 追加列 → 写出
    lib.rs                    # Tauri 命令层
  examples/gen_test_file.rs   # 测试数据生成 + 端到端验证
src/
  App.vue                     # 前端界面（Vue 3 + TS）
```

## 算法验证记录

log 解密算法（`BrCipherMaker`）已通过三层验证：

1. Java 原实现 ↔ Python 移植版 44 个测试向量双向互解（覆盖全部 10 把密钥、含空格/中文边界）
2. 生产库 19 条真实密文 → 真实手机号（含 `Uw0JCΒ6goHAVhTV1Q` → `15023709720`，固化为回归测试）
3. Rust 移植版 11 个单元测试（含上述向量）+ 20 万行端到端验证 **准确率 100%**
