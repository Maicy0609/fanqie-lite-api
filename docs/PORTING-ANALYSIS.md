# 移植分析：fqapp 后端 → fanqie-lite-api

> 上游：`E:\0\fqapp`（什锦铺），master @ `4eaeac3`，GPL-3.0
> 目标：独立后端服务，前后端分离，多平台前端可复用，可接入第三方工具
> 本文只做移植分析与接口设计，不含业务实现代码
> 遵循 `docs/CODING.md`：最小改动、优先复用、严控依赖、尊重原项目

> **进度**：Phase 0（骨架移植）✓　Phase 1（HTTP 服务壳）✓　Phase 2（数据目录与设备池）✓
> Phase 3（媒体）✓　Phase 4（日志与部署）✓　Phase 5（接口文档、参考客户端、CI）✓
> Phase 6（客户端双模式）✓　Phase 7（管理页）✓
> 实际偏离与验证记录见 `docs/UPSTREAM.md`。

---

## 1. 移植边界

### 1.1 做什么

把 `fqapp/rust` 这个 crate 变成可独立部署的 HTTP 服务，保持业务行为不变。

```
        原形态                              目标形态
┌──────────────────────┐            ┌─────────────────────────┐
│ Flutter App          │            │       任意前端          │
│  ├ ApiClient (1732)  │            │  Web / Flutter / CLI    │
│  └ BackendTransport  │            │  第三方工具             │
│       ↓ FFI         │            │       ↓ HTTP           │
│ ┌──────────────────┐ │            │ ┌─────────────────────┐ │
│ │ fqapi_core       │ │            │ │ fqapi-core          │ │  ← 原样搬
│ │ + loopback:18921 │ │            │ │ (业务代码不动)      │ │
│ └──────────────────┘ │            │ ├─────────────────────┤ │
└──────────────────────┘            │ │ fqapi-server        │ │  ← 新写
                                    │ │ 鉴权/CORS/限流/日志 │ │
                                    │ └─────────────────────┘ │
                                    └─────────────────────────┘
```

### 1.2 不做什么（本版明确排除）

| 项 | 原因 |
|---|---|
| 用户账号体系 / 登录 | 原项目产品边界明确"不持有账号"，加了就是另一个产品 |
| 书架 / 进度云同步 | 需要 DB + 用户体系，前端本地已有，塞进来会破坏无状态 |
| 内容 DTO 归一化层 | 前端现有解析器已兼容上游形状，再加一层只增加维护面 |
| JS 响应过滤器 | 原项目 `filter.rs` 是空壳，`enabled=true` 即启动失败；不做 |
| 视频转码 / HLS 重封装 | 超出"代理 + 解密"范围 |
| 管理后台 | 先用 `/healthz` + 日志，不做 |

### 1.3 一个必须早定的原则

**服务端保持无状态。**

原项目的用户数据（书架、进度、划线、设置）全在 Flutter 本地 Hive，Rust 核心只持有两样东西：设备池文件、`src/` 媒体缓存。移植后继续保持——不引入数据库。这样部署是"一个二进制 + 一个数据目录"，备份就是拷目录。

一旦为了"多端同步"引入 DB，复杂度会翻倍，且不再是"移植"。

---

## 2. 代码资产盘点

`rust/src` 共 18,566 行（不含 codegen 产物 `frb_generated.rs`）。

### 2.1 可原样搬（约 17,600 行，95%）

| 模块 | 行数 | 说明 |
|---|---:|---|
| `endpoints/` | ~12,000 | 46 条路由 + 全部业务逻辑，**一行不改** |
| `sign/` | ~1,300 | argus / ladon / gorgon / simon / sm3 / protobuf |
| `upstream/` | ~800 | 带签名 HTTP、设备轮换、CM/DH 握手 |
| `device/` | ~1,600 | 设备池、四步注册、池文件加密（仅 key 来源要改） |
| `crypto.rs` | 314 | AES-CBC / PKCS7 / gzip 上限 |
| `dispatch.rs` | 288 | **核心资产**：`Request → Response` 已经是 HTTP 形状 |
| `static_files.rs` | 236 | 受限静态文件 + Range/HEAD/416 |
| `json.rs` / `error.rs` / `timeutil.rs` / `config.rs` / `filter.rs` | ~370 | 工具层 |

`dispatch.rs` 的签名是这次移植能成立的关键：

```rust
pub async fn dispatch(server: &Server, req: &Request) -> Response
pub struct Request { method, path, query, body, headers }
pub struct Response { status, content_type, headers, body }
```

它本身就是一个 axum handler 的抽象。所以"换传输层"不需要动业务。

### 2.2 必须改写（约 500–900 行，3–5%）

| 文件 | 行数 | 处理 |
|---|---:|---|
| `api/mod.rs` | 920 | **删掉 FFI 门面，保留运行时**。`CORE` 单例、`INFLIGHT`、`cancel`、`race_dispatch`、`generation` 全部有用；`init`/`request`/`base_url`/`session_capability` 是 FFI 专用，删 |
| `server.rs` | 332 | **重写**。loopback 的 capability 模型不适用于公开服务；Host/Origin 反跨站逻辑要反过来变成 CORS 允许清单 |
| `core_log.rs` | 280 | 保留 `RotatingFile`（约 200 行），删掉 `android_logger` 分支（约 60 行），加 stdout 输出 |
| `lib.rs` | 39 | 删 `mod frb_generated;`、`#[cfg(windows)] mod desktop_media;` |
| `Cargo.toml` | — | 删 `flutter_rust_bridge`、`android_logger`；`crate-type` 改回 `rlib` |
| `device/pool_crypto.rs` | 272 | 只改**调用点**：key 从 Dart 的 Keystore 改为服务端自管 |

### 2.3 不搬

| 资产 | 原因 |
|---|---|
| `api/mod.rs` 的 FFI 部分（`init`/`request`/`cancel`/`status`/`base_url`/`session_capability`） | 无 FRB |
| `frb_generated.rs`（约 2,000 行生成代码） | 无 FRB |
| `desktop_media.rs`（481 行） | Windows libmpv 专用 |
| `native/`（CENC C 库 + JNI） | 给 Android ExoPlayer 用的；服务端用 `video.rs` 里已有的 Rust CENC 实现 |
| `flutter_rust_bridge` / `android_logger` 依赖 | 平台绑定 |
| `tower` / `tower-http` 依赖声明 | **原项目里根本没用到**（grep 零引用），服务端可以真正用上，也可以删 |

### 2.4 测试资产必须整体搬

`rust/tests/`（3,859 行）+ 各模块内嵌单测 + `rust/testdata/` 6 份夹具：

```
crypto_vectors.rs        argus/ladon/sm3/simon 黄金向量
device_pool*.rs          池容量 / 加密 / 无密钥 / 损坏备份 / retire 竞态
loopback_guard.rs        Host / Origin / capability
static_and_loopback.rs   Range / HEAD / 416 / 路径逃逸
mock_upstream_contract.rs 本地 mock 上游跑真实签名请求（离线可测）
router_contract.rs       46 条路由的路径匹配契约
video_codec_gate.rs      编码门
manga_end_to_end.rs      图片解密端到端
```

这些是**移植正确性的唯一锚点**。判断"搬得对不对"的标准就是：**移植后 `cargo test` 全绿，且测试文件一个字符都不改**。做不到，说明搬错了。

---

## 3. 七个移植阻断点

按"不改就跑不起来"排序。每一条都给出最小改法。

### 3.1 设备池密钥的来源

**现状**：`api/mod.rs::set_device_pool_key` 由 Dart 侧 `SecureKeyProvider` 从 Android Keystore 取 32B 传入；`pool_crypto` 用 `AES-256-CBC + HMAC-SHA256` 加密 `device_pool.json`（magic `FQPOOL1`）。无 key 时退化为明文 + 一次 warn。

**服务端没有 Keystore。**

最小改法：在 `fqapi-server` 启动时按优先级取 key，调同一个 `provision_pool_key`：

```
1. 环境变量 FQ_POOL_KEY（64 位 hex）
2. <data-dir>/pool.key（首次启动自动生成，权限 0600）
3. 都没有 → 生成并写 pool.key
```

**不要改 `pool_crypto.rs` 内部**。它的"加密文件永不被明文覆盖"逻辑（先 rename 成 `.corrupt-<ts>` 再写）是对的，改它只会引入新 bug。

### 3.2 运行时目录的解析

**现状**：`api/mod.rs::resolve_runtime_dir` 依赖 `config/config.json` 的父目录，找不到就退回配置所在目录。

**最小改法**：`fqapi-server` 显式接收 `--data-dir`（默认 `./data`），启动时创建：

```
<data-dir>/
├─ config/config.json      # 缺失时写入内置默认
├─ config/filter.json      # 缺失时写入全 disabled
├─ config/device_pool.json
├─ pool.key
├─ src/                    # 媒体缓存
└─ logs/rust.log
```

然后把绝对路径传给 `Config::load` 和 `Ctx`。`resolve_runtime_dir` 保持原样不动（传空 `runtime_dir` 时它会正确推导）。

### 3.3 鉴权模型整体失效

**现状**：`server.rs` 的 `capability` 是 per-launch 32B 随机 token，目的是"同机其他 App 不能冒名发评论"。只对 3 条写路由（`playlet_comment_add` / `digg` / `reply`）强制，读路由完全开放（为了让 Legado 能用）。

**服务端暴露后，这个模型不成立**：谁能连上端口，谁就能用你的设备身份发评论。

最小改法（新写 `fqapi-server/src/auth.rs`）：

| 层 | 规则 |
|---|---|
| 默认 | 所有 `/api/v1/*` 要求 `Authorization: Bearer <token>` |
| 多 token | `FQ_API_TOKENS` 逗号分隔，支持给不同客户端发不同 token，便于单独吊销 |
| 只读放行 | `--anonymous-read` 时读路由免鉴权（给 Legado 这类无法加 header 的场景） |
| 写路由 | **永远**要求 token，不受 `--anonymous-read` 影响 |
| 常量时间比较 | token 比较用 `subtle` 风格逐字节异或，避免时序侧信道（不引依赖，20 行） |

保留原 `dispatch::requires_capability()` 不动——它仍然准确描述"哪些是写路由"，直接复用这个函数决定"必须鉴权"的集合。

### 3.4 `/src/*` 的语义变了

**现状**：`ctx.src_dir` 是**每台设备自己的缓存**，里面是解密后的漫画图和视频。`/src/img_<sha256>.jpg` 是明文。

**服务端共享一个 `src_dir`**，会带来三个问题：

1. 服务端在托管解密后的版权内容（原项目刻意把内容留在用户设备上）
2. 无界增长（原项目的 512 MiB 预算 + LRU 是单用户量级）
3. 任何人猜到 URL 就能取

**这是架构决策点，必须在写代码前定。** 三个选项：

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| **A 透传（推荐默认）** | `video` 返回 CDN URL + `key_hex`；`manga` 返回图片 URL 列表 | 服务端不碰内容，零存储，零责任 | 客户端必须能解 CENC / GCM，Web 端做不到 |
| **B 代理** | `/media/v/{id}` `/media/img/{sha256}` 流式代理，带签名 token + TTL | 多平台统一，客户端零解密 | 服务端承担带宽与内容责任 |
| **C 落盘** | 保留现状 + TTL + 预算 + 不可枚举文件名 | 实现最简单 | 最接近"内容分发者"，不建议 |

建议：**A 为默认，B 为可选（`--media-proxy`）**，C 只在单实例自用场景开。

若走 B，服务端已有现成能力：
- 视频 CENC：`video.rs::decrypt_cenc_mp4`（Rust 实现，box 解析 + CTR）
- 漫画 GCM：`manga.rs::decrypt_manga_image`（手写 GCM）
- Range/HEAD/416：`static_files.rs`

即"把 `src_dir` 换成一个带 TTL 的临时目录"，其余不动。

### 3.5 `api/mod.rs` 删到什么程度

保留（这些与服务端强相关）：

```
static CORE / INIT_LOCK / GENERATION
static INFLIGHT / PRE_CANCELLED
cancel()            ← 客户端断开时取消上游请求
race_dispatch()     ← 取消 + deadline 竞争
shutdown()          ← SIGTERM 优雅退出，等 pool.retire()
init_core()         ← 去掉 mock_origin 之外几乎可原样用
```

删除（FFI 专用）：

```
BridgeResponse / request() / base_url() / session_capability()
status()            → 换成 /healthz
set_device_pool_key() → 换成 server 侧读 key 后直接调 provision_pool_key
```

建议：把保留部分挪成 `fqapi-core/src/runtime.rs`，`api` 模块整体删除。这样 `core` crate 里不再出现任何"桥"的概念。

### 3.6 日志双写的平台分支

**现状**：`core_log::install` 只在 `#[cfg(target_os = "android")]` 调用，同时写 logcat 和 `rust.log`（1 MiB 轮转，1 份备份）。

**最小改法**：保留 `RotatingFile`，去掉 `LOGCAT_TAG` 分支，`install` 在服务端无条件调用，并同时输出到 stdout（容器场景必需）。手写约 40 行 `log::Log` 实现，**不引入 `tracing` / `env_logger`**（依赖纪律）。

日志格式保持原样 `[级别] 标签: 消息`，与 Flutter 侧 `app.log` 同形——这样前端日志页还能解析服务端日志。

### 3.7 客户端断开的传播

**现状**：Dart 侧取消 → `rust.cancel(id)` → `CancellationToken`。

**服务端**：axum handler 的 future 被 drop 时，需要在 drop 里 cancel。最小做法是复用一个 `CancellationToken`，用 `Drop` guard 触发：

```
handler(req) → token
  spawn/await: race_dispatch(&token, deadline, dispatch(...))
  DropGuard: on drop → token.cancel()
```

这样"用户关掉页面"会连带取消正在等待的上游请求和重试退避，与原项目语义一致。`race_dispatch` 现成可用。

---

## 4. 值得注意的问题（不要犯同样错误）

按处理方式分三级。**"必修"指不修会在服务端放大成事故。**

### 4.1 必修

#### (1) `pool.get()` 每个请求写一次盘

`device/mod.rs`：

```rust
// get() 末尾
let _ = self.save().await;      // ← 每次取设备都落盘
// get_by_id() 同样
```

而 `UpstreamClient::do_with_device_inner` **每个上游请求**都会调 `pool.get()` 或 `get_by_id()`。也就是说：

> **每一个业务请求 = 一次 `device_pool.json` 全量序列化 + 加密 + tmp 写 + rename。**

单用户手机上无所谓；作为服务，QPS 10 就是 10 次/秒的加密 + fsync 级写盘，池文件还会随设备数增长。

**最小改法**：把 `last_used` / `use_count` 的持久化从"每次"改成"脏标记 + 后台 flush"：

- `get()` / `get_by_id()` 只改内存 + 置 `dirty`
- 后台任务每 30s 或每 N 次变更 flush 一次
- 池结构变化（新增 / 替换 / 淘汰）仍然立即落盘，保证崩溃后身份不丢

注意：这会改变原项目的落盘时机，`device_pool*.rs` 的测试可能需要补一个"flush 后可读"的用例。属于**必要的偏离**，要在 `UPSTREAM.md` 里登记。

#### (2) 设备池容量与并发的关系

池 5–20 个身份，上游按 `device_id` 限流。原项目是"1 个用户 + 16 并发闸门"。

服务端如果不限流，N 个客户端同时打，会把 20 个身份全部打到限流，然后触发 `DeviceFailed` → `replace_failed` → 每个失败设备最多 5 次重新注册（每次含 2s sleep）→ 池子被洗一遍，雪崩。

**最小改法**：全局 `tokio::sync::Semaphore`，许可数 = `池大小 × 3`（默认 60），在进入 `dispatch` 前获取。池耗尽时**排队**而不是报错。不引入新依赖。

#### (3) 冷启动 10–20 秒

`registrar.register()` 内部有 `sleep(2s)`（模拟真实行为，删了 `registerKey` 会失败），加上 4 个 HTTP 往返和 3 次重试。池最小 5 个 → 冷启动约 10–20 秒。

原项目用"启动页 + 15s 轮询 `/health`"掩盖了这件事。服务端必须显式处理：

- `/healthz` 立即 200（进程活着）
- `/readyz` 在池里至少有 1 个可用设备前返回 503
- 启动时后台 warm-up，不阻塞监听
- 反向代理/K8s 用 `/readyz` 做就绪探针

### 4.2 建议改（小改动，收益明确）

| # | 问题 | 位置 | 改法 |
|---|---|---|---|
| 4 | `eprintln!` 绕过 `log` 宏 | `video.rs::handle_pseries` 5 处 | 改 `log::warn!`。否则 release 版这些诊断永远不进 `rust.log` |
| 5 | `reading724_params` 四份拷贝 | `home.rs` / `comments.rs` / `audio.rs` / `book.rs` | 先**不动**。合并需要逐字段比对 + 字节级回归，收益低于风险。登记为技术债 |
| 6 | 签名常量分散 | `sign/argus.rs` 三个常量 + `device/crypto.rs` 两个 | 先不动。但要在文档里列成"上游换 key 时需要改的 5 个常量"清单，避免下次满仓库找 |
| 7 | `config.admin` 完全未使用 | `config.rs` | 死配置。保留结构（不影响行为），文档里标注"未实现" |
| 8 | `anti_crawler` 只在 404 时 302 | `dispatch.rs` | 服务端无意义（会把 404 变成重定向）。保持 `enabled=false`，不删代码 |
| 9 | `HOST_FQNOVEL` 与 `HOST_FQNOVELB` 值相同 | `endpoints/base.rs` | 保留（纯命名冗余），不合并 |
| 10 | 错误只有 400/500 两类 | `error.rs` | 建议加可选 `code` 字段（`bad_request` / `upstream_failed` / `device_unavailable` / `decrypt_failed`），多端可据此决定是否重试。**向后兼容**，老客户端忽略即可 |
| 11 | `tower` / `tower-http` 声明未用 | `Cargo.toml` | 服务端正好需要（CORS、静态文件）；要么真正用上，要么删声明 |

### 4.3 仅记录（不改）

| 问题 | 为什么不改 |
|---|---|
| `version_code=66.9` 这类非法值 | 刻意对齐上游 Go 实现，改了必 403 |
| 设备指纹（UA / device_type / resolution）散落各 handler | 上游改版时必须逐处跟进；抽常量反而让人误以为可以统一改 |
| 账号 Cookie 采集路径 | 见 5.3，默认不启用 |
| `filter.rs` 空壳 | 保持 disabled，行为与原来一致 |

---

## 5. 接口设计

### 5.1 路径：完全保留

**46 条 `/api/v1/*` 路径一条不改。**

这是整个移植最大的收益：现有 Flutter 前端只需把 `RustBackendTransport` 换成 `HttpBackendTransport` + 一个 base URL，`ApiClient` 的 1,732 行**一行不用改**。任何"重新设计 RESTful 路径"的想法都会让这个收益归零。

新增（不占用 `/api/v1` 命名空间）：

| 路径 | 用途 |
|---|---|
| `GET /healthz` | 进程存活，恒 200，返回版本 + 设备数 |
| `GET /readyz` | 池内至少 1 个可用设备才 200，否则 503 |
| `GET /metrics` | 可选，Prometheus 文本格式（手写，不加依赖） |

### 5.2 响应契约：保持透传

- 成功：`200` + 上游 JSON **原样**（键名、层级、`code` 字段都不动）
- 失败：`{"success":false,"error":"..."}`，HTTP 400（参数）/ 500（其他）
- 取消：`499`，超时：`504`（与原项目一致）

**不做 DTO 归一化。** 前端 `parseDirectory` / `parseHomeFeed` 等解析器已经同时兼容上游形状和旧桥接形状，服务端再转一层只会制造第三个形状。

唯一建议的增量：错误信封加可选 `code`（见 4.2 #10）。

### 5.3 媒体

见 3.4。接口形态：

```
# 透传模式（默认）
GET /api/v1/videos/{id}          → { video_url, key_hex, variants[] }
GET /api/v1/manga/{id}           → { images: [cdn_url...], encrypt_key? }

# 代理模式（--media-proxy）
GET /media/v/{id}                → 206 + CENC 解密后的 MP4 流（支持 Range）
GET /media/img/{sha256}.jpg      → 200 + 解密后的图片（签名 token，TTL 24h）
```

代理 URL 必须带签名 token（`?t=<hmac>`），否则等于开放一个匿名图床。

### 5.4 认证

```
Authorization: Bearer <token>
```

| 场景 | 行为 |
|---|---|
| 默认 | 全部 `/api/v1/*` 需要 token |
| `--anonymous-read` | 只读路由免 token；3 条写路由仍需 |
| 媒体代理 | 需要 token 或 URL 签名 |

### 5.5 账号采集路径（`account/fanqie/*`）

**建议默认关闭**，通过 `--enable-account-capture` 显式开启。

原因：这两个端点接收用户 Cookie 并转发给上游。原项目"不持有账号"的边界是靠"只在本机内存里用"守住的；一旦上服务端，日志、抓包、备份、多租户都可能让 Cookie 落盘，性质就变了。

若必须开，硬约束：

- Cookie 只允许出现在 POST body（原样保留 `account_library.rs` 的校验）
- 不写任何日志（现有代码已经做到，`capture_failure` 不带 cookie）
- 不落盘、不缓存、不跨请求复用
- 只在内存中驻留一次请求的生命周期

---

## 6. 运行时模型

### 6.1 并发与限流

```
客户端 → axum
  → 鉴权
  → 全局 Semaphore（默认 60 = 池 20 × 3）
  → 可选每 IP 滑窗（默认关）
  → dispatch → upstream（设备轮换，最多 3 次）
  → 上游
```

超时分层（沿用原项目数值，不要改）：

| 层 | 值 | 来源 |
|---|---|---|
| 上游 HTTP | 15s | `HttpTransport::new(15)` |
| 注册 HTTP | 30s | `Registrar` |
| 账号采集 | 20s | `without_redirects(20)` |
| 客户端整体 | 由请求方 `timeout_ms` 决定 | 前端默认 20s |

### 6.2 设备池

- 单例 `Arc<DevicePool>`，进程内唯一（与原项目一致）
- 容量 5–20，LRU
- 后台 warm-up：启动时补齐到 5，之后每 10 分钟检查一次
- `retire()` 在 SIGTERM 时调用，等写盘完成再退出

### 6.3 缓存

| 内容 | 预算 | 淘汰 |
|---|---|---|
| 视频缓存 | 512 MiB | LRU（原逻辑 `prune_src_cache`） |
| 漫画图 | 同上（共享 `src/`） | 内容哈希命名 + LRU |
| 池文件 | 无 | — |

代理模式下把 `src_dir` 指到临时目录，启动时清空。

### 6.4 优雅退出

```
SIGTERM
 → 停止接受新连接（axum graceful shutdown）
 → 等在途请求完成（上限 30s）
 → pool.retire().await      ← 等已提交的写盘完成，之后的写全部拒绝
 → 退出
```

`shutdown()` 的语义原项目已经实现对了，直接复用。

---

## 7. 目标目录结构

```
fanqie-lite-api/
├─ Cargo.toml                    # workspace
├─ crates/
│  ├─ fqapi-core/                # 从 fqapp/rust 搬，业务代码不动
│  │  ├─ Cargo.toml              # 删 FRB / android_logger
│  │  ├─ src/
│  │  │  ├─ lib.rs               # 删 frb_generated / desktop_media
│  │  │  ├─ runtime.rs           # ← 由 api/mod.rs 瘦身而来（新增）
│  │  │  ├─ dispatch.rs          # 原样
│  │  │  ├─ sign/                # 原样
│  │  │  ├─ upstream/            # 原样
│  │  │  ├─ device/              # 仅 key 来源 + 写盘频率
│  │  │  ├─ endpoints/           # 原样
│  │  │  ├─ crypto.rs            # 原样
│  │  │  ├─ static_files.rs      # 原样
│  │  │  └─ ...
│  │  ├─ testdata/               # 原样（6 份夹具）
│  │  └─ tests/                  # 原样（3,859 行）
│  └─ fqapi-server/              # 新增
│     ├─ src/
│     │  ├─ main.rs              # CLI / 信号 / 启动
│     │  ├─ http.rs              # axum router + dispatch 适配
│     │  ├─ auth.rs              # Bearer + 常量时间比较
│     │  ├─ cors.rs              # 允许清单
│     │  ├─ limit.rs             # 全局信号量
│     │  ├─ media.rs             # 代理模式（可选）
│     │  └─ health.rs            # /healthz /readyz /metrics
│     └─ tests/                  # 冒烟：真实起服务 + curl
├─ docs/
│  ├─ CODING.md
│  ├─ backend-analysis.md        # 已完成
│  ├─ PORTING-ANALYSIS.md        # 本文
│  ├─ API.md                     # 接口手册（移植后写）
│  └─ UPSTREAM.md                # 上游 commit + 偏离清单
├─ deploy/
│  └─ Dockerfile
└─ scripts/
   └─ smoke.sh
```

**为什么两个 crate 而不是一个**：`fqapi-core` 要能独立 `cargo test` 全绿，证明"业务代码确实没改"；`fqapi-server` 只做传输与运维。合成一个 crate 会让 `--all-features` 互相干扰，也不好 diff 上游。

---

## 8. 移植阶段与验收

每个阶段都必须满足 `CODING.md` 的双门槛：**`cargo clippy -- -D warnings` 通过 + 全量 `cargo test` 全绿**。

### Phase 0 — 骨架（不改行为）

1. 建 workspace
2. 复制 `rust/src` → `crates/fqapi-core/src`，`testdata`、`tests` 一并复制
3. 改 `Cargo.toml`（删 FRB / android_logger，`crate-type = ["rlib"]`）
4. `lib.rs` 删两行 mod
5. `api/mod.rs` → `runtime.rs`（删 FFI 函数，保留 CORE/INFLIGHT/cancel/race_dispatch）
6. `server.rs` 先**原样保留**（能编译即可，后面替换）
7. `core_log.rs` 去掉 android 分支

**验收**：`cargo test -p fqapi-core` 全绿，且 `tests/` 与 `testdata/` 零改动。

> 这一步是整个移植的成败关键。测试全绿 = 业务逻辑搬对了；任何测试需要改，都说明改错了东西。

### Phase 1 — 服务壳

1. `fqapi-server`：CLI（手写 env/arg 解析，不加 clap）、axum router、`dispatch` 适配
2. 鉴权、CORS、全局限流
3. `/healthz` `/readyz`
4. 客户端断开 → `CancellationToken`

**验收**：`curl /api/v1/search?query=xx` 返回上游 JSON；`curl /healthz` 200；未带 token 返回 401；写路由带 token 可用。

### Phase 2 — 数据目录与设备池

1. `--data-dir` + 目录初始化
2. pool key 三级来源
3. 写盘频率改造（4.1 #1）
4. 后台 warm-up + 定时补齐

**验收**：首次启动自动注册 5 个设备（10–20s），`readyz` 转 200；重启复用池文件不重新注册；`kill -TERM` 优雅退出。

### Phase 3 — 媒体

1. 透传模式（默认）：确认 `video` / `manga` 返回契约
2. 代理模式（可选）：`/media/v/{id}`、`/media/img/{sha256}`
3. URL 签名 + TTL

**验收**：Range 请求返回 206 + `Content-Range`；过期 token 返回 403；缓存预算生效。

### Phase 4 — 观测与部署

1. stdout 日志 + `rust.log` 轮转
2. `/metrics`（可选）
3. Dockerfile（rustls + webpki-roots，**不需要 ca-certificates**，可上 distroless）
4. `scripts/smoke.sh`

**验收**：容器冷启动到 `readyz` 200；日志可读；镜像体积可接受。

### Phase 5 — 文档

1. `docs/API.md`：46 条路由的参数表（从 `router.rs` 逐条整理）
2. `docs/UPSTREAM.md`：上游 commit `4eaeac3` + 所有偏离项
3. 前端接入示例（Flutter / curl）

---

## 9. 风险与合规

### 9.1 许可证（硬约束）

原项目是 **GPL-3.0**。移植后的衍生作品**必须**：

- 同样以 GPL-3.0 发布
- 保留原作者的版权声明和许可证全文
- 分发二进制时提供对应源码

GPL-3.0 不是 AGPL：只提供服务而不分发二进制，不触发源码开放义务。但**一旦分发**（发 Docker 镜像、发二进制）就必须开源。如果打算闭源商业化，这条路走不通，需要重新评估。

### 9.2 上游风控

服务端出口 IP 集中，比单设备更容易被识别。20 个设备身份共享一个 IP，风控可能按 IP 而非 device_id 拦截。

这不是本项目能解决的（需要出口 IP 池），但要在部署文档里写清楚：**单实例适合个人/小团队，不适合对外大规模开放**。

### 9.3 内容责任

- 透传模式：服务端不存储、不转码内容，只转发 URL 和 key → 风险最低
- 代理/落盘模式：服务端成为内容分发节点 → 风险显著上升

建议默认透传，代理作为"给 Web 端的兼容选项"，且明确说明用途。

### 9.4 账号 Cookie

见 5.5。默认关闭。

### 9.5 签名常量失效

`sign/argus.rs` 的三个常量 + `device/crypto.rs` 的两个常量是这套方案的地基。上游换 key 就必须重新逆向。要在 `UPSTREAM.md` 里维护一张"失效时的 5 个常量"清单，并保留 `rust/testdata/vectors.json` 作为"换 key 后如何验证"的对照。

### 9.6 单点

无 HA。池、缓存、密钥都是进程内状态。多实例部署会各自注册设备、各自缓存，上游看到的是 N 倍的设备注册量——反而更容易触发风控。**明确不支持多实例横向扩展**，写进部署文档。

---

## 10. 结论与建议

1. **可行，且改动面很小**：18,566 行里只有 500–900 行（3–5%）需要改写，业务代码 95% 原样搬。`dispatch.rs` 的 `Request/Response` 抽象让"换传输层"几乎零成本。

2. **成败锚点是 Phase 0**：移植后 `cargo test` 必须全绿且测试文件零改动。这是判断"有没有改坏业务"的唯一可靠标准。

3. **三个必须在写代码前拍板的决策**：
   - 媒体：透传（默认）/ 代理（可选）/ 落盘（不建议）
   - 账号采集：默认关闭
   - 部署规模：单实例，不支持横向扩展

4. **两个必修的性能/稳定性问题**：`pool.get()` 每请求写盘、池容量与并发不匹配。不修会在服务端放大成事故。

5. **接口设计的第一原则是"不改路径"**：46 条 `/api/v1/*` 原样保留，现有前端只换 base URL。任何"重新设计 RESTful 风格"的冲动都会让这次移植的收益归零。

6. **尊重原项目的最小做法**：不重构 `reading724_params` 的四份拷贝、不合并重复常量、不"修正" `version_code=66.9`。把这些登记为技术债，而不是在移植期顺手清理——移植期混入重构，出问题时无法区分是"搬错了"还是"改坏了"。
