# 解码宝匣 DecodeBox — 迭代手册

> 供后续迭代时快速上手：技术栈全景、双系统发布流程、Git 账号隔离。最后更新：2026-09-02。

---

## 一、项目一句话

跨平台桌面工具（macOS / Windows）：导入 xlsx/csv，批量解密加密手机号（log / md5 / sha256），在原列右侧追加解密结果，失败数据单独落文件。产品名**解码宝匣 DecodeBox**，本地路径：

```
/Users/callmedana/work/projects/phone-decrypt-tool
```

---

## 二、技术栈全景

### 架构图

```
┌──────────── 前端（WebView 渲染）────────────┐
│ Vue 3.5 + TypeScript 5.6 + Vite 6          │
│ GSAP 3（弹窗 / 首页入场动画）               │
└──────────────┬──────────────────────────────┘
               │ Tauri IPC（invoke 命令 + event 事件）
┌──────────────▼──────────────────────────────┐
│ Rust 核心（tauri 2.11）                     │
│  src/cipher.rs      log 解密算法移植         │
│  src/detect.rs      密文类型识别             │
│  src/grpc_client.rs tonic gRPC 远程查表     │
│  src/pipeline.rs    文件读取/解密/追加列/写出 │
│  src/lib.rs         Tauri 命令层            │
└──────────────────────────────────────────────┘
```

### 分层明细

| 层 | 技术 | 说明 |
|---|---|---|
| 桌面壳 | Tauri 2.11 | 系统WebView，包体 ~7MB（Electron 同类 100MB+） |
| 前端 | Vue 3.5（`<script setup>`）+ TS | 界面与交互 |
| 动画 | GSAP 3 core | 弹窗 back.out 入场、首页 stagger；全部尊重 `prefers-reduced-motion` |
| 解密算法 | 纯 Rust（无框架依赖） | `BrCipherMaker` 从 Java 移植，19 条真实生产密文回归测试 |
| gRPC | tonic 0.13 + prost + **protox** | protox 编译 proto 无需系统装 protoc |
| Excel 读 | calamine 0.28 | 纯 Rust 读 xlsx |
| Excel 写 | rust_xlsxwriter 0.79（constant_memory） | 常量内存写 20w 行 |
| CSV | csv 1.3 + encoding_rs | 流式 + GBK/UTF-8/UTF-16 自动识别 |

### 三个"加密方式"的本质（勿混淆）

| 方式 | 本质 | 实现 |
|---|---|---|
| log 密文（含 Β/Α） | 本地可逆（XOR/移位+Base64） | 纯本地，离线可用 |
| md5 / sha256 | 单向哈希 | 远程查表 `grpc.brapp.com`（**需 VPN**） |

**gRPC 两个坑（已踩平，改动前必读）**：
1. 地址 `http://grpc.brapp.com` **不能带端口**——显式 `:443` 被 LB 拒绝
2. JSON 字段名 `"alogrithm"`（服务端固有拼写错误，勿改成 algorithm）

### 关键业务规则（用户确认过的约束）

- 单列文件，解密第 **1 列（A 列）**，表头名任意
- 第一行固定当表头，数据从第二行开始
- 解密类型在**上传时**选定（整文件一种方式）
- 失败数据单独落文件：字段与原文一致（原表头+原始行，格式跟随原文）
- 「打开文件」按钮（openPath）**已删除**——跨平台权限坑；只用「打开所在目录」（revealItemInDir）

---

## 三、开发环境

### mac（日常开发机）

```bash
# 前置：node ≥18、rustup（已装）
cd /Users/callmedana/work/projects/phone-decrypt-tool
npm install                # 首次/依赖变更时
npm run tauri dev          # 开发模式（热更新）
npm run tauri build        # 打 mac 包
cargo test --lib           # 单元测试（含 19 条真实密文回归）
cargo test --lib -- --ignored  # 真机 gRPC 集成测试（需 VPN）
```

mac 产物：
```
src-tauri/target/release/bundle/macos/解码宝匣.app
src-tauri/target/release/bundle/dmg/解码宝匣_0.1.0_aarch64.dmg
```

### Windows（备用构建机，工具链已装齐）

已装：Node 20 / WebView2 / MSVC Build Tools 14.44 / Rust（rsproxy 镜像）。

```powershell
cd C:\projects\phone-decrypt-tool   # 每次先从 mac 同步最新代码
npm install
npm run tauri build
```

Windows 产物：
```
src-tauri\target\release\bundle\nsis\解码宝匣_0.1.0_x64-setup.exe   # 安装包
src-tauri\target\release\bundle\msi\*.msi                           # 备选安装包
src-tauri\target\release\phone-decrypt-tool.exe                     # 免安装单文件绿色版
```

### mac 上跑不了的

mac **无法**交叉编译 Windows 安装包（MSVC 链接器闭源）。双系统包只能靠方案 A（云端）或方案 B（Windows 机）。

---

## 四、Git 账号隔离（个人账号，不用公司 Git）

**现状**：本仓库已配置**仓库级**个人身份（不切换全局 git 账号，公司项目零影响）：

```bash
# 已配置（仅 phone-decrypt-tool 仓库内生效）
git config user.name  "callmedana"
git config user.email "869377908@qq.com"
```

**认证方式：gh CLI（已安装并登录）**，push 走 gh 的 credential helper，无需 SSH key、无需密码。

日常推送：

```bash
cd /Users/callmedana/work/projects/phone-decrypt-tool
git push          # 凭据由 gh 管理，无需任何输入
```

**验证隔离**：

```bash
git config user.name          # 本仓库：callmedana ✓
git config --global user.name  # 全局：hong.chen（公司身份未动）✓
```

---

## 五、发布方案（双系统安装包）

### 方案 A：GitHub Actions 云构建（长期主方案，推荐）

**原理**：mac 上改代码 → `git push` → GitHub 云机器编译三平台 → Actions 页面下载。mac 不编译 Windows。

**已配好**：`.github/workflows/build.yml`（三平台矩阵 + rust-cache 缓存加速），随仓库首次提交（7600310）入库，push 即生效。

**首次启用**：
1. GitHub 建私有仓库 `decode-box`
2. 按上面第四节配好个人账号 remote
3. `git push -u origin main`

**日常迭代循环**：

```bash
# 改代码 → 测试 → 提交 → 推送
cargo test --lib && npx vue-tsc --noEmit   # 本地门禁
git add -A && git commit -m "描述"
git push

# 发正式版（Actions 会自动出三平台包并挂到 GitHub Releases）
git tag v0.2.0
git push origin v0.2.0
```

**取包位置**：仓库 → Actions → 对应构建 → Artifacts 下载（或 Releases 页面）。
**构建时长**：三平台约 10-15 分钟（有 rust-cache 后续会快很多）。

⚠ **合规提醒**：仓库含 log 解密密钥与内网服务凭据（cipher.rs + grpc_client.rs 硬编码）。推 GitHub 必须**建私有仓库**；若公司代码合规不允许外网托管，改用公司 GitLab（需运维配 Windows runner）或纯方案 B。

### 方案 B：Windows 本地构建（短期 / 兜底）

**流程**：
```
mac 改代码 → 压缩传 Windows（U盘/微信）→ 解压 → npm install && npm run tauri build → 取 bundle\nsis\*.exe
```

**打包干净源码（mac 上执行）**：

```bash
cd /Users/callmedana/work/projects
rm -f decode-box-src.zip
zip -rq decode-box-src.zip phone-decrypt-tool \
  -x "phone-decrypt-tool/src-tauri/target/*" \
  -x "phone-decrypt-tool/node_modules/*" \
  -x "phone-decrypt-tool/dist/*" \
  -x "phone-decrypt-tool/src-tauri/gen/*"
```

**Windows 上的提示词**（粘给那台机器上的 AI 助手）：

```text
构建本目录（C:\projects\phone-decrypt-tool）的 Tauri 2 应用 Windows 版。
代码已完成，只构建，不改业务代码。环境此前已装好（Node 20 / MSVC / Rust）。
步骤：
1. cd C:\projects\phone-decrypt-tool
2. npm install
3. npm run tauri build（首次编译约 5-15 分钟，勿中断）
4. 完成后告诉我产物路径：
   - 安装包 bundle\nsis\*_setup.exe
   - 免安装版 phone-decrypt-tool.exe（可改名"解码宝匣.exe"，单文件绿色版）
报错时贴完整错误信息。
```

### 两个方案怎么选

| 场景 | 用哪个 |
|---|---|
| 日常迭代（改代码→发版） | A：push 完等 15 分钟取包 |
| 临时紧急修一个包 | B：比等 CI 快（Windows 机就绪时） |
| GitHub 不可用/合规限制 | B |
| 同时要 mac + win 包 | A 一条命令全出；B 要两边各跑一次 |

**建议**：A 配好后 B 作应急。两者不互斥。

---

## 六、发版检查清单（每次迭代）

```
□ cargo test --lib 全过（含 19 条真实密文回归）
□ npx vue-tsc --noEmit 类型检查过
□ mac 本地 npm run tauri dev 冒烟（导入→试解→执行→弹窗）
□ 版本号三处同步：
    src-tauri/tauri.conf.json 的 version
    package.json 的 version
    src-tauri/Cargo.toml 的 version
□ git commit + push
□ (方案A) 打 tag 发版； (方案B) 压源码包传 Windows
□ 新安装包归档到版本目录（保留"已知好的包"作基线）
□ 重要架构变化更新本文档
```

---

## 七、常见问题速查

| 问题 | 答案 |
|---|---|
| 表头必须叫 phone_encrypted？ | 不必，任意名（认第 1 列位置） |
| 第一行不是表头？ | 固定跳过第一行；无表头文件手动加一行任意表头 |
| md5/sha256 解密失败？ | 确认 VPN（内网 grpc.brapp.com）；弹窗有黄色提醒 |
| 「打开文件」没了？ | 已删（跨平台权限问题），用「打开所在目录」 |
| Windows 下载 Rust 慢？ | rsproxy 镜像（RUSTUP_DIST_SERVER=https://rsproxy.cn）+ cargo 镜像（~/.cargo/config.toml） |
| dmg 和 .app 区别？ | .app 是应用本体；.dmg 是分发镜像（拖入应用程序） |
| 免安装版？ | release 目录的裸 exe（win）/ 直接用 .app（mac） |
