# kiro-rs

一个用 Rust 编写的 Anthropic Claude API 兼容代理服务，将 Anthropic API 请求转换为 Kiro API 请求。

> **本仓库是 [hank9999/kiro.rs](https://github.com/hank9999/kiro.rs) 的 fork**，主要在原项目基础上修复了几个跑久了才暴露的负载均衡 / 缓存问题。详见下方 [Fork 与上游区别](#fork-与上游区别)。

---

## Fork 与上游区别

相对 [hank9999/kiro.rs v1.1.30](https://github.com/hank9999/kiro.rs) 的主要改动：

- **P0#1 retry 不再撞同一个凭据**：原版 affinity 短路会让失败凭据被反复选回（实测 100 burst 切换率 0%）。Fork 在 retry 链路加 `exclude_ids` 强制跳过上次失败凭据。
- **P0#2 新凭据「雷暴防护」**：新凭据加入时 `recent_usage=0`，LB 立刻判它为「最少使用」→ 1 秒内被打 47 次 429。Fork 用现有凭据 `recent_usage` 中位数作 baseline。
- **P0#3 周期 balance 刷新**：原版实测 24h 0 条 balance refresh log，cache 完全是启动时快照。Fork 加 10 分钟周期刷新 + 余额不足主动禁用。
- **P0#4 cache_tracker TTL 对齐上游**：原版命中时刷新 `expires_at`，但 Anthropic 真实 TTL 从首次写入算。Fork 修复后 `cache_read` 数字与上游真实命中率一致。
- **credentialRpm: 0 真禁用本地限流**：原版 `0` 会落回默认 1-2 秒间隔 + 每日 500 上限（与字面意思相反）。Fork 让 `0` 真的跳过所有本地限流检查。
- **凭据级 IdP/代理/Admin Portal 设置**：Admin UI 支持编辑凭据级 Auth/API Region、IdC `clientId/clientSecret`、HTTP/SOCKS5 代理与 `direct` 直连覆盖。
- **Overages 在线启停与状态同步**：Admin UI 支持 SSE 实时开启/关闭 Overages；后端会从 Web Portal 与 `GetUserUsageAndLimits` 同步 overage 状态，并持久化到凭据。
- **Overage-aware 余额与自动禁用**：余额展示、缓存与自动禁用逻辑使用“基础额度 + 超额额度”的有效额度；上游未返回 `overageEnabled` 时不会误判为关闭。
- **Admin 详情页白屏修复与全局模型列表**：修复凭据详情渲染问题；固定 `/v1/models` 能力列表不再在每个凭据详情重复显示，改为 Admin 顶部全局“可用模型”。
- **Thinking 兼容增强**：`claude-opus-4-7-thinking` 与 `claude-opus-4-6-thinking` 一样走 adaptive thinking；客户端即使选择不带 `-thinking` 的模型，只要请求带 `thinking` 参数也会启用思考。
- **Release 与 Docker Hub 自动构建**：保留多平台二进制 release workflow，并在 push tag `v*` 时自动构建 Docker Hub 镜像。

镜像：`foxfishs/kiro-rs:latest`（不含本次修复的请用上游镜像 `ghcr.io/hank9999/kiro-rs:latest`）。

---

## 免责声明

本项目仅供研究使用, Use at your own risk, 使用本项目所导致的任何后果由使用人承担, 与本项目无关。
本项目与 AWS/KIRO/Anthropic/Claude 等官方无关, 本项目不代表官方立场。

## 注意！

因 TLS 默认从 native-tls 切换至 rustls，你可能需要专门安装证书后才能配置 HTTP 代理。可通过 `config.json` 的 `tlsBackend` 切回 `native-tls`。
如果遇到请求报错, 尤其是无法刷新 token, 或者是直接返回 error request, 请尝试切换 tls 后端为 `native-tls`, 一般即可解决。

**Write Failed/会话卡死**: 如果遇到持续的 Write File / Write Failed 并导致会话不可用，参考 Issue [#22](https://github.com/hank9999/kiro.rs/issues/22) 和 [#49](https://github.com/hank9999/kiro.rs/issues/49) 的说明与临时解决方案（通常与输出过长被截断有关，可尝试调低输出相关 token 上限）

## 功能特性

- **Anthropic API 兼容**: 完整支持 Anthropic Claude API 格式
- **流式响应**: 支持 SSE (Server-Sent Events) 流式输出
- **Token 自动刷新**: 自动管理和刷新 OAuth Token
- **多凭据支持**: 支持配置多个凭据，按优先级自动故障转移
- **智能重试**: 单凭据最多重试 2 次，单请求最多重试 3 次
- **凭据回写**: 多凭据格式下自动回写刷新后的 Token
- **Thinking 模式**: 支持 Claude 的 extended thinking 功能
- **客户端 Thinking 参数兼容**: 模型名不带 `-thinking` 时，只要请求携带 `thinking` 参数也会启用思考；无效预算默认 high
- **工具调用**: 完整支持 function calling / tool use
- **WebSearch**: 内置 WebSearch 工具转换逻辑
- **多模型支持**: 支持 Sonnet、Opus、Haiku 系列模型
- **Admin 管理**: 可选的 Web 管理界面和 API，支持凭据管理、余额查询、Overages 启停、凭据级配置等
- **多级 Region 配置**: 支持全局和凭据级别的 Auth Region / API Region 配置
- **凭据级代理**: 支持为每个凭据单独配置 HTTP/SOCKS5 代理，优先级：凭据代理 > 全局代理 > 无代理
- **Overage-aware 额度**: 支持读取、缓存并展示基础额度 + 超额额度的有效余额，避免误禁用已开启 Overages 的凭据

---

- [Fork 与上游区别](#fork-与上游区别)
- [开始](#开始)
  - [1. 下载预编译二进制文件](#1-下载预编译二进制文件)
  - [2. 从源码编译](#2-从源码编译)
  - [3. 最小配置](#3-最小配置)
  - [4. 启动](#4-启动)
  - [5. 验证](#5-验证)
  - [Docker](#docker)
- [配置详解](#配置详解)
  - [config.json](#configjson)
  - [credentials.json](#credentialsjson)
  - [Region 配置](#region-配置)
  - [代理配置](#代理配置)
  - [认证方式](#认证方式)
  - [环境变量](#环境变量)
- [API 端点](#api-端点)
  - [标准端点 (/v1)](#标准端点-v1)
  - [Thinking 模式](#thinking-模式)
  - [工具调用](#工具调用)
- [模型映射](#模型映射)
- [Admin（可选）](#admin可选)
  - [Admin UI 功能](#admin-ui-功能)
  - [Admin API](#admin-api)
- [注意事项](#注意事项)
- [项目结构](#项目结构)
- [技术栈](#技术栈)
- [License](#license)
- [致谢](#致谢)

## 开始

### 1. 下载预编译二进制文件

如果不需要改代码，推荐直接从 Release 下载预编译二进制文件：

[https://github.com/Foxfishc/kiro.rs/releases/](https://github.com/Foxfishc/kiro.rs/releases/)

根据系统下载对应文件：

- Linux x86_64：`kiro-rs-linux-x86_64.tar.gz`
- macOS Apple Silicon / aarch64：`kiro-rs-macos-aarch64.tar.gz`
- Windows x86_64：`kiro-rs-windows-x86_64.zip`

> macOS 当前提供 Apple Silicon / aarch64 版本。如果你是 Intel Mac，需要暂时从源码编译。

Linux 解压并运行：

```bash
curl -L -o kiro-rs-linux-x86_64.tar.gz \
  https://github.com/Foxfishc/kiro.rs/releases/download/v1.1.33/kiro-rs-linux-x86_64.tar.gz

tar -xzf kiro-rs-linux-x86_64.tar.gz
cd kiro-rs-linux-x86_64
chmod +x kiro-rs
./kiro-rs -c /path/to/config.json --credentials /path/to/credentials.json
```

macOS Apple Silicon 解压并运行：

```bash
curl -L -o kiro-rs-macos-aarch64.tar.gz \
  https://github.com/Foxfishc/kiro.rs/releases/download/v1.1.33/kiro-rs-macos-aarch64.tar.gz

tar -xzf kiro-rs-macos-aarch64.tar.gz
cd kiro-rs-macos-aarch64
chmod +x kiro-rs
./kiro-rs -c /path/to/config.json --credentials /path/to/credentials.json
```

如果 macOS 提示来自未知开发者，可以执行：

```bash
xattr -dr com.apple.quarantine ./kiro-rs
```

Windows 解压并运行 PowerShell 示例：

```powershell
Invoke-WebRequest `
  -Uri "https://github.com/Foxfishc/kiro.rs/releases/download/v1.1.33/kiro-rs-windows-x86_64.zip" `
  -OutFile "kiro-rs-windows-x86_64.zip"

Expand-Archive -Path "kiro-rs-windows-x86_64.zip" -DestinationPath "." -Force
cd kiro-rs-windows-x86_64
.\kiro-rs.exe -c C:\path\to\config.json --credentials C:\path\to\credentials.json
```

你也可以把二进制放进系统 PATH，然后在任意目录运行：

```bash
kiro-rs -c /path/to/config.json --credentials /path/to/credentials.json
```

### 2. 从源码编译

> **前置步骤**：编译前需要先构建前端 Admin UI（用于嵌入到二进制中）：
> ```bash
> cd admin-ui && pnpm install && pnpm build
> ```

```bash
cargo build --release
```

### 3. 最小配置

创建 `config.json`：

```json
{
   "host": "127.0.0.1",
   "port": 8990,
   "apiKey": "sk-kiro-rs-qazWSXedcRFV123456",
   "region": "us-east-1"
}
```
> PS: 如果你需要 Web 管理面板, 请注意配置 `adminApiKey`

创建 `credentials.json`（从 Kiro IDE 等中获取凭证信息）：
> PS: 可以前往 Web 管理面板配置跳过本步骤
> 如果你对凭据地域有疑惑, 请查看 [Region 配置](#region-配置)

Social 认证：
```json
{
   "refreshToken": "你的刷新token",
   "expiresAt": "2025-12-31T02:32:45.144Z",
   "authMethod": "social"
}
```

IdC 认证：
```json
{
   "refreshToken": "你的刷新token",
   "expiresAt": "2025-12-31T02:32:45.144Z",
   "authMethod": "idc",
   "clientId": "你的clientId",
   "clientSecret": "你的clientSecret"
}
```

### 4. 启动

生产/嵌入式 Admin UI：

```bash
./target/release/kiro-rs
```

开发模式（推荐调试 Admin UI 时使用）：

```bash
make dev
```

- 前端开发地址：`http://localhost:5173/admin/`
- 前端 `/api` 请求会通过 Vite 代理到：`http://localhost:8990`
- 后端直连地址：`http://localhost:8990/admin`

或指定配置文件路径：

```bash
./target/release/kiro-rs -c /path/to/config.json --credentials /path/to/credentials.json
```

### 5. 验证

```bash
curl http://127.0.0.1:8990/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: sk-kiro-rs-qazWSXedcRFV123456" \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 1024,
    "stream": true,
    "messages": [
      {"role": "user", "content": "Hello, Claude!"}
    ]
  }'
```

### Docker

**使用预构建镜像（推荐）**

本 fork 的镜像发布在 Docker Hub：

```bash
docker pull foxfishs/kiro-rs:latest
# 或指定版本
docker pull foxfishs/kiro-rs:v1.1.33
```

支持 `linux/amd64` 和 `linux/arm64` 双架构，每次 push tag `v*` 时由 GitHub Actions 自动构建。构建依赖仓库 Secrets：

- `DOCKERHUB_USERNAME`：Docker Hub 用户名
- `DOCKERHUB_TOKEN`：Docker Hub Access Token

手动本地构建并推送示例：

```bash
# 登录 Docker Hub
docker login

# 单平台本地构建
docker build -t <你的DockerHub用户名>/kiro-rs:latest .
docker push <你的DockerHub用户名>/kiro-rs:latest

# 多平台构建并推送（推荐）
docker buildx create --use --name kiro-rs-builder 2>/dev/null || docker buildx use kiro-rs-builder
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -t <你的DockerHub用户名>/kiro-rs:latest \
  -t <你的DockerHub用户名>/kiro-rs:v1.1.33 \
  --push .
```

如果使用 GitHub Actions 自动构建，只需要在 GitHub 仓库 Settings → Secrets and variables → Actions 中配置上面两个 Docker Hub secret，然后推送 `v*` tag。

**docker-compose 方式**

```bash
# 准备 config/config.json 和 config/credentials.json
docker compose up -d
```

> 注意：仓库自带的 `docker-compose.yml` 默认拉取 `ghcr.io/hank9999/kiro-rs:latest`（**不含本 fork 的修复**）。要用 fork 版镜像，把 `image:` 改为 `foxfishs/kiro-rs:latest`，或改用 `build: .` 本地构建。

**本地构建**

```bash
docker compose up -d --build
```

需要将 `config.json` 和 `credentials.json` 挂载到容器中，具体参见 `docker-compose.yml`。

## 配置详解

### config.json

| 字段 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `host` | string | `127.0.0.1` | 服务监听地址 |
| `port` | number | `8080` | 服务监听端口 |
| `apiKey` | string | - | 自定义 API Key（用于客户端认证，必配） |
| `region` | string | `us-east-1` | AWS 区域 |
| `authRegion` | string | - | Auth Region（用于 Token 刷新），未配置时回退到 region |
| `apiRegion` | string | - | API Region（用于 API 请求），未配置时回退到 region |
| `kiroVersion` | string | `0.10.0` | Kiro 版本号 |
| `machineId` | string | - | 自定义机器码（64位十六进制），不定义则自动生成 |
| `systemVersion` | string | 随机 | 系统版本标识 |
| `nodeVersion` | string | `22.21.1` | Node.js 版本标识 |
| `tlsBackend` | string | `rustls` | TLS 后端：`rustls` 或 `native-tls` |
| `countTokensApiUrl` | string | - | 外部 count_tokens API 地址 |
| `countTokensApiKey` | string | - | 外部 count_tokens API 密钥 |
| `countTokensAuthType` | string | `x-api-key` | 外部 API 认证类型：`x-api-key` 或 `bearer` |
| `proxyUrl` | string | - | HTTP/SOCKS5 代理地址 |
| `proxyUsername` | string | - | 代理用户名 |
| `proxyPassword` | string | - | 代理密码 |
| `adminApiKey` | string | - | Admin API 密钥，配置后启用凭据管理 API 和 Web 管理界面 |
| `credentialRpm` | number | - | 单凭据目标 RPM（每分钟请求数），用于凭据级节流/分流；`0` 或未配置表示使用内置默认策略 |
| `promptCacheTtlSeconds` | number | `300` | 本地 Prompt Cache TTL（秒） |
| `promptCacheAccountingEnabled` | boolean | `true` | 是否启用本地 Prompt Cache usage 记账；关闭后不再输出或扣减 cache token |

完整配置示例：

```json
{
   "host": "127.0.0.1",
   "port": 8990,
   "apiKey": "sk-kiro-rs-qazWSXedcRFV123456",
   "region": "us-east-1",
   "tlsBackend": "rustls",
   "kiroVersion": "0.10.0",
   "machineId": "64位十六进制机器码",
   "systemVersion": "darwin#24.6.0",
   "nodeVersion": "22.21.1",
   "authRegion": "us-east-1",
   "apiRegion": "us-east-1",
   "countTokensApiUrl": "https://api.example.com/v1/messages/count_tokens",
   "countTokensApiKey": "sk-your-count-tokens-api-key",
   "countTokensAuthType": "x-api-key",
   "proxyUrl": "http://127.0.0.1:7890",
   "proxyUsername": "user",
   "proxyPassword": "pass",
   "adminApiKey": "sk-admin-your-secret-key",
   "credentialRpm": 5,
   "promptCacheTtlSeconds": 300,
   "promptCacheAccountingEnabled": true
}
```

### credentials.json

支持单对象格式（向后兼容）或数组格式（多凭据）。

#### 字段说明

| 字段             | 类型     | 描述                                          |
|----------------|--------|---------------------------------------------|
| `id`           | number | 凭据唯一 ID（可选，仅用于 Admin API 管理；手写文件可不填）        |
| `accessToken`  | string | OAuth 访问令牌（可选，可自动刷新）                        |
| `refreshToken` | string | OAuth 刷新令牌                                  |
| `profileArn`   | string | AWS Profile ARN（可选，登录时返回）                   |
| `expiresAt`    | string | Token 过期时间 (RFC3339)                        |
| `authMethod`   | string | 认证方式：`social` 或 `idc`                       |
| `clientId`     | string | IdC 登录的客户端 ID（IdC 认证必填）                     |
| `clientSecret` | string | IdC 登录的客户端密钥（IdC 认证必填）                      |
| `priority`     | number | 凭据优先级，数字越小越优先，默认为 0                         |
| `region`       | string | 凭据级 Auth Region, 兼容字段                       |
| `authRegion`   | string | 凭据级 Auth Region，用于 Token 刷新, 未配置时回退到 region |
| `apiRegion`    | string | 凭据级 API Region，用于 API 请求                    |
| `machineId`    | string | 凭据级机器码（64位十六进制）                             |
| `email`        | string | 用户邮箱（可选，从 API 获取）                           |
| `proxyUrl`     | string | 凭据级代理 URL（可选，特殊值 `direct` 表示不使用代理）       |
| `proxyUsername`| string | 凭据级代理用户名（可选）                                |
| `proxyPassword`| string | 凭据级代理密码（可选）                                 |
| `overageEnabled` | boolean | 是否已知开启 Overages（由 Admin/Web Portal/usage 查询自动同步，也可手动保留） |
| `overageCap`   | number | Overages 超额额度上限（未配置时使用默认值）                 |

说明：
- IdC / Builder-ID / IAM 在本项目里属于同一种登录方式，配置时统一使用 `authMethod: "idc"`
- 为兼容旧配置，`builder-id` / `iam` 仍可被识别，但会按 `idc` 处理

#### 单凭据格式（旧格式，向后兼容）

```json
{
   "accessToken": "请求token，一般有效期一小时，可选",
   "refreshToken": "刷新token，一般有效期7-30天不等",
   "profileArn": "arn:aws:codewhisperer:us-east-1:111112222233:profile/QWER1QAZSDFGH",
   "expiresAt": "2025-12-31T02:32:45.144Z",
   "authMethod": "social",
   "clientId": "IdC 登录需要",
   "clientSecret": "IdC 登录需要"
}
```

#### 多凭据格式（支持故障转移和自动回写）

```json
[
   {
      "refreshToken": "第一个凭据的刷新token",
      "expiresAt": "2025-12-31T02:32:45.144Z",
      "authMethod": "social",
      "priority": 0
   },
   {
      "refreshToken": "第二个凭据的刷新token",
      "expiresAt": "2025-12-31T02:32:45.144Z",
      "authMethod": "idc",
      "clientId": "xxxxxxxxx",
      "clientSecret": "xxxxxxxxx",
      "region": "us-east-2",
      "priority": 1,
      "proxyUrl": "socks5://proxy.example.com:1080",
      "proxyUsername": "user",
      "proxyPassword": "pass"
   },
   {
      "refreshToken": "第三个凭据（显式不走代理）",
      "expiresAt": "2025-12-31T02:32:45.144Z",
      "authMethod": "social",
      "priority": 2,
      "proxyUrl": "direct"
   }
]
```

多凭据特性：
- 按 `priority` 字段排序，数字越小优先级越高（默认为 0）
- 单凭据最多重试 3 次，单请求最多重试 9 次
- 自动故障转移到下一个可用凭据
- 多凭据格式下 Token 刷新后自动回写到源文件
- Admin UI 修改凭据级 Region、IdC、代理或 Overages 状态后会自动回写到源文件

### Region 配置

支持多级 Region 配置，分别控制 Token 刷新和 API 请求使用的区域。

**Auth Region**（Token 刷新）优先级：
`凭据.authRegion` > `凭据.region` > `config.authRegion` > `config.region`

**API Region**（API 请求）优先级：
`凭据.apiRegion` > `config.apiRegion` > `config.region`

### 代理配置

支持全局代理和凭据级代理，凭据级代理会覆盖该凭据产生的所有出站连接（API 请求、Token 刷新、额度查询）。

**代理优先级**：`凭据.proxyUrl` > `config.proxyUrl` > 无代理

| 凭据 `proxyUrl` 值 | 行为 |
|---|---|
| 具体 URL（如 `http://proxy:8080`、`socks5://proxy:1080`） | 使用凭据指定的代理 |
| `direct` | 显式不使用代理（即使全局配置了代理） |
| 未配置（留空） | 回退到全局代理配置 |

凭据级代理示例：

```json
[
   {
      "refreshToken": "凭据A：使用自己的代理",
      "authMethod": "social",
      "proxyUrl": "socks5://proxy-a.example.com:1080",
      "proxyUsername": "user_a",
      "proxyPassword": "pass_a"
   },
   {
      "refreshToken": "凭据B：显式不走代理（直连）",
      "authMethod": "social",
      "proxyUrl": "direct"
   },
   {
      "refreshToken": "凭据C：使用全局代理（或直连，取决于 config.json）",
      "authMethod": "social"
   }
]
```

### 认证方式

客户端请求本服务时，支持两种认证方式：

1. **x-api-key Header**
   ```
   x-api-key: sk-your-api-key
   ```

2. **Authorization Bearer**
   ```
   Authorization: Bearer sk-your-api-key
   ```

### 环境变量

可通过环境变量配置日志级别：

```bash
RUST_LOG=debug ./target/release/kiro-rs
```

## API 端点

### 标准端点 (/v1)

| 端点 | 方法 | 描述 |
|------|------|------|
| `/v1/models` | GET | 获取可用模型列表 |
| `/v1/messages` | POST | 创建消息（对话） |
| `/v1/messages/count_tokens` | POST | 估算 Token 数量 |

### Thinking 模式

支持 Claude 的 extended thinking 功能：

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 16000,
  "thinking": {
    "type": "enabled",
    "budget_tokens": 10000
  },
  "messages": [...]
}
```

Thinking 配置规则：

- 模型名带 `-thinking` 后缀时会自动启用 thinking。
- 支持强度后缀：`-thinking-minimal`、`-thinking-low`、`-thinking-medium`、`-thinking-high`、`-thinking-xhigh`。
- 对 `claude-opus-4-6-thinking`、`claude-opus-4-7-thinking`、`claude-sonnet-4-6-thinking` 使用 Kiro 侧需要的 `adaptive` thinking，并设置 `output_config.effort = "high"`。
- 模型名不带 `-thinking` 时，只要客户端请求携带 `thinking` 参数也会启用思考；`budget_tokens` 使用客户端传入值。
- 如果客户端携带 `thinking` 但预算缺失、为 `0` 或无效，则默认按 high 使用 `24576`。

### 工具调用

完整支持 Anthropic 的 tool use 功能：

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 1024,
  "tools": [
    {
      "name": "get_weather",
      "description": "获取指定城市的天气",
      "input_schema": {
        "type": "object",
        "properties": {
          "city": {"type": "string"}
        },
        "required": ["city"]
      }
    }
  ],
  "messages": [...]
}
```

## 模型映射

`GET /v1/models` 返回当前服务固定暴露的模型能力列表；该列表不按单个凭据动态区分。Admin UI 顶部“可用模型”弹窗展示的也是这个全局列表。

| Anthropic 模型 | Kiro 模型 |
|----------------|-----------|
| `*sonnet*`（含 4-6/4.6） | `claude-sonnet-4.6` |
| `*sonnet*`（其他） | `claude-sonnet-4.5` |
| `*opus*`（含 4-5/4.5） | `claude-opus-4.5` |
| `*opus*`（其他） | `claude-opus-4.6` |
| `*haiku*` | `claude-haiku-4.5` |

## Admin（可选）

当 `config.json` 配置了非空 `adminApiKey` 时，会启用 Web 管理界面与 Admin API。Admin 认证同时支持 `x-api-key` 和 `Authorization: Bearer`。

### Admin UI 功能

- `GET /admin` - 访问管理页面（需要在编译前构建 `admin-ui/dist`）。
- 凭据列表：查看启用状态、优先级、邮箱、Region、代理、余额、Overages 状态与失败计数。
- 凭据详情：查看账号信息、余额、Region、IdC 与代理配置；避免在每个凭据详情重复展示固定模型列表。
- 凭据编辑：支持修改凭据级 Auth/API Region、IdC `clientId/clientSecret`、HTTP/SOCKS5 代理；`proxyUrl: "direct"` 表示该凭据显式直连。
- Overages 管理：支持从 Admin UI 发起开启/关闭 Overages，并通过 SSE 展示实时进度；成功后状态会同步到凭据文件。
- 余额管理：单凭据实时查询余额；列表页使用缓存余额，缓存包含有效总额度、剩余额度、Overages 开关和超额上限。
- 全局可用模型：顶部“可用模型”展示 `/v1/models` 暴露的固定模型集合。该列表是服务能力列表，不按单个凭据区分。

### Admin API

- `GET /api/admin/credentials` - 获取所有凭据状态
- `POST /api/admin/credentials` - 添加新凭据
- `DELETE /api/admin/credentials/:id` - 删除凭据
- `POST /api/admin/credentials/:id/disabled` - 设置凭据禁用状态
- `POST /api/admin/credentials/:id/priority` - 设置凭据优先级
- `POST /api/admin/credentials/:id/region` - 设置凭据 Region
- `POST /api/admin/credentials/:id/reset` - 重置失败计数
- `GET /api/admin/credentials/:id/balance` - 实时获取凭据余额并更新缓存
- `GET /api/admin/credentials/balances/cached` - 获取所有凭据缓存余额
- `GET /api/admin/credentials/:id/overage/status` - 获取凭据 Overages 状态
- `POST /api/admin/credentials/:id/overage/enable` - 开启 Overages（SSE）
- `POST /api/admin/credentials/:id/overage/disable` - 关闭 Overages（SSE）
- `POST /api/admin/credentials/:id/portal-settings` - 更新凭据级 Admin Portal 设置（Region / IdC / 代理等）

## 注意事项

1. **凭证安全**: 请妥善保管 `credentials.json` 文件，不要提交到版本控制
2. **Token 刷新**: 服务会自动刷新过期的 Token，无需手动干预
3. **WebSearch 工具**: 只要 `tools` 中包含 `web_search`（按 name 或 type 判断），就走内置 WebSearch 处理逻辑

## 项目结构

```
kiro-rs/
├── src/
│   ├── main.rs                 # 程序入口
│   ├── http_client.rs          # HTTP 客户端构建
│   ├── token.rs                # Token 计算模块
│   ├── debug.rs                # 调试工具
│   ├── test.rs                 # 测试
│   ├── model/                  # 配置和参数模型
│   │   ├── config.rs           # 应用配置
│   │   └── arg.rs              # 命令行参数
│   ├── anthropic/              # Anthropic API 兼容层
│   │   ├── router.rs           # 路由配置
│   │   ├── handlers.rs         # 请求处理器
│   │   ├── middleware.rs       # 认证中间件
│   │   ├── types.rs            # 类型定义
│   │   ├── converter.rs        # 协议转换器
│   │   ├── stream.rs           # 流式响应处理
│   │   └── websearch.rs        # WebSearch 工具处理
│   ├── kiro/                   # Kiro API 客户端
│   │   ├── provider.rs         # API 提供者
│   │   ├── token_manager.rs    # Token 管理
│   │   ├── machine_id.rs       # 设备指纹生成
│   │   ├── model/              # 数据模型
│   │   │   ├── credentials.rs  # OAuth 凭证
│   │   │   ├── events/         # 响应事件类型
│   │   │   ├── requests/       # 请求类型
│   │   │   ├── common/         # 共享类型
│   │   │   ├── token_refresh.rs # Token 刷新模型
│   │   │   └── usage_limits.rs # 使用额度模型
│   │   └── parser/             # AWS Event Stream 解析器
│   │       ├── decoder.rs      # 流式解码器
│   │       ├── frame.rs        # 帧解析
│   │       ├── header.rs       # 头部解析
│   │       ├── error.rs        # 错误类型
│   │       └── crc.rs          # CRC 校验
│   ├── admin/                  # Admin API 模块
│   │   ├── router.rs           # 路由配置
│   │   ├── handlers.rs         # 请求处理器
│   │   ├── service.rs          # 业务逻辑服务
│   │   ├── types.rs            # 类型定义
│   │   ├── middleware.rs       # 认证中间件
│   │   └── error.rs            # 错误处理
│   ├── admin_ui/               # Admin UI 静态文件嵌入
│   │   └── router.rs           # 静态文件路由
│   └── common/                 # 公共模块
│       └── auth.rs             # 认证工具函数
├── admin-ui/                   # Admin UI 前端工程（构建产物会嵌入二进制）
├── tools/                      # 辅助工具
├── Cargo.toml                  # 项目配置
├── config.example.json         # 配置示例
├── docker-compose.yml          # Docker Compose 配置
└── Dockerfile                  # Docker 构建文件
```

## 技术栈

- **Web 框架**: [Axum](https://github.com/tokio-rs/axum) 0.8
- **异步运行时**: [Tokio](https://tokio.rs/)
- **HTTP 客户端**: [Reqwest](https://github.com/seanmonstar/reqwest)
- **序列化**: [Serde](https://serde.rs/)
- **日志**: [tracing](https://github.com/tokio-rs/tracing)
- **命令行**: [Clap](https://github.com/clap-rs/clap)

## License

MIT

## 致谢

本 fork 在 [hank9999/kiro.rs](https://github.com/hank9999/kiro.rs) 的基础上做了若干负载均衡与缓存修复，地基由原作者打就，向原作者致敬。

原项目的实现也离不开前辈的努力:
 - [kiro2api](https://github.com/caidaoli/kiro2api)
 - [proxycast](https://github.com/aiclientproxy/proxycast)

部分逻辑参考了以上项目, 再次由衷的感谢!

## 友情链接

本项目在 [LINUX DO](https://linux.do) 公益推广，[LINUX DO](https://linux.do) 是一个真诚、友善、团结、专业的新型综合性社区，欢迎来玩。
