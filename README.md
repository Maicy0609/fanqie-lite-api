# fanqie-lite-api

把 [fqapp](https://github.com/ch6vip/fqapp) 的后端（Rust 核心）移植为**独立 HTTP 服务**，
让前端可以多平台复用、前后端分离、并接入第三方工具。

当前状态：**Phase 3 完成**（骨架移植 + HTTP 服务壳 + 数据目录与设备池 + 媒体）。
业务代码 95% 原样搬自上游。

## 快速开始

```bash
cargo run -p fqapi-server -- --token change-me --data-dir ./data
```

```bash
curl http://127.0.0.1:18921/healthz
curl -H "Authorization: Bearer change-me" \
     "http://127.0.0.1:18921/api/v1/search?query=%E5%89%91%E6%9D%A5"
```

首次启动会生成 `data/pool.key`，并在后台注册 5 个上游设备身份，约 20 秒；期间
`/readyz` 返回 503。没有 `--token` 会直接拒绝启动 —— 一个不鉴权的监听端口等于把本机的
设备身份借给任何人。

### 数据目录

```
data/
├─ config/config.json       # 缺失时写入默认值，不会覆盖已有文件
├─ config/filter.json       # 同上
├─ config/device_pool.json  # 设备池，AES-256-CBC + HMAC-SHA256 加密
├─ pool.key                 # 池加密密钥（64 hex 字符）
├─ src/                     # 媒体缓存
└─ rust.log                 # 核心日志，1 MiB 轮转
```

**`pool.key` 丢了就等于丢了全部设备身份**：服务端会拒绝启动而不是重新生成密钥
（新密钥打不开旧池，池会以空状态加载，然后重注册一批新设备）。请和 `device_pool.json`
一起备份。也可以用 `FQ_POOL_KEY` 从外部注入，此时不需要 `pool.key` 文件。

### 参数

| 参数 | 默认 | 说明 |
|---|---|---|
| `--data-dir` | `./data` | 设备池、媒体缓存、日志、池密钥 |
| `--listen` | `127.0.0.1:18921` | 监听地址 |
| `--token` | — | API 令牌，可重复或用逗号分隔 |
| `--anonymous-read` | 关 | 放行只读路由（给 Legado 这类加不了 header 的客户端） |
| `--max-concurrency` | `60` | 在途请求上限，超出的排队 |
| `--timeout` | `60` | 单请求秒数上限 |
| `--cors-origin` | 无 | 允许的跨域源，可重复或用逗号分隔 |

同名环境变量为 `FQ_DATA_DIR`、`FQ_LISTEN`、`FQ_API_TOKENS`、`FQ_ANONYMOUS_READ`、
`FQ_MAX_CONCURRENCY`、`FQ_TIMEOUT_SECS`、`FQ_CORS_ORIGINS`、`FQ_POOL_KEY`。

### 鉴权

所有 `/api/v1/*` 默认要求 `Authorization: Bearer <token>`。三条例外：

- `/healthz`、`/readyz` 永远开放（探针没有令牌）
- `--anonymous-read` 下只读路由开放，但**设备绑定写路由**（发评论、发弹幕、点赞）与
  `/api/v1/account/*` 仍必须带令牌
- `/src/*`（缓存媒体）也永远要令牌 —— 它是内容本身，不是元数据，`--anonymous-read`
  不应该把已缓存的剧集公开出去

### 媒体

服务端有两条媒体路径，由客户端按请求选择，不需要服务端开关：

| 路径 | 行为 | 适用 |
|---|---|---|
| `GET /api/v1/videos/{id}?mode=stream` | 返回 CDN 地址 + `key_hex`，**服务端不落地** | 客户端能自己解 CENC（原生 App） |
| `GET /api/v1/videos/{id}` | 服务端下载、解密、缓存，返回 `/src/{id}.mp4` | 客户端不想管解密；支持离线 |
| `GET /api/v1/manga/{id}` | 服务端解密每张图，返回 `/src/img_{sha256}.jpg` | 漫画没有透传模式，图片是 AES-GCM 加密 |

缓存统一放在 `<data-dir>/src`，预算 512 MiB，超出按最久未用淘汰（`.part` 下载中的文件
不参与统计）。`/src/*` 支持 `Range`，可以直接喂给 `<video>` 或播放器。

```bash
# 浏览器/标签发不了 header，媒体路径接受 query 形式的凭证
curl -H "Range: bytes=0-1023" \
     "http://127.0.0.1:18921/src/<id>.mp4?token=change-me"
```

`?token=` 只在 `/src/*` 生效，打 API 会被拒绝。响应带 `Referrer-Policy: no-referrer`
和 `Cache-Control: private`，避免这条 URL 从 referrer 或共享缓存里漏出去。

## 部署（Docker）

```bash
export FQ_API_TOKENS=$(openssl rand -hex 32)
docker compose -f deploy/docker-compose.yml up -d --build
curl -fsS http://127.0.0.1:18921/healthz
```

不用 compose 时：

```bash
docker build -t fqapi-lite-api:local .
docker run -d --name fqapi -p 127.0.0.1:18921:18921 \
  -v fqapi-data:/data -e FQ_API_TOKENS="$FQ_API_TOKENS" \
  fqapi-lite-api:local
```

镜像以非 root（uid 10001）运行，`/data` 是卷。**卷要备份**：里面有 `pool.key`，
丢了就等于丢了全部设备身份。

跑一遍容器冒烟（不需要网络）：

```bash
scripts/smoke.sh fqapi-lite-api:local
```

它会验证 `/healthz` 通、匿名请求一律 401、有效令牌能过、`?token=` 打不开 API 路由；
`/readyz` 只报告不判定——就绪要等上游注册，取决于网络。

镜像里带 `LICENSE` 与 `NOTICE`：GPL-3.0 第 6 条，分发镜像就是分发二进制。

## 日志

两个出口，同一行格式：

| 出口 | 内容 |
|---|---|
| stdout | 核心日志 + 访问日志，容器直接 `docker logs` 看 |
| `<data-dir>/rust.log` | 只有核心日志，1 MiB 轮转、1 份备份 |

访问日志一行一请求：`[I] <iso> access: GET /api/v1/search 200 42ms`。
它**故意不写文件**——文件只有 1 MiB，访问日志会把要留的诊断挤掉。
也**不记录 query string**，因为媒体 URL 里带令牌。

## 目录

```
crates/fqapi-core/     # 从 fqapp/rust 移植的核心（业务代码 95% 原样）
  src/runtime/         # 进程级状态：init / shutdown / 取消 / 超时
  src/dispatch.rs      # 唯一分发器（Request -> Response，HTTP 形状）
  src/endpoints/       # 46 条 /api/v1/* 路由与业务 handler
  src/sign/            # x-argus / x-ladon / x-gorgon / simon / sm3
  src/device/          # 设备池 + 四步注册 + 池文件加密
  src/upstream/        # 带签名的 HTTP、设备轮换、CM/DH 握手
  tests/ testdata/     # 与上游逐字节一致，是移植正确性的锚点
crates/fqapi-server/   # HTTP 外壳（新增）
  src/config.rs        # 命令行 / 环境变量
  src/poolkey.rs       # 池密钥的解析、生成与迁移判定
  src/http.rs          # axum 路由、dispatch 适配、流式文件、CORS
  src/auth.rs          # Bearer 鉴权 + 常量时间比较
  src/health.rs        # /healthz /readyz
  src/main.rs          # 启动顺序、预热、信号与优雅退出
clients/python/        # 参考客户端（纯标准库）+ 端到端脚本
scripts/               # smoke.sh（容器冒烟）、check_api_doc.py（文档契约）
deploy/                # docker-compose 示例
.github/workflows/     # CI：fmt+clippy / test / 文档契约 / e2e / 镜像
docs/
  CODING.md              # 实现原则
  API.md                 # 46 个端点、57 条路径的接口手册
  backend-analysis.md    # 上游后端逻辑分析
  PORTING-ANALYSIS.md    # 移植方案、接口设计、阶段计划
  UPSTREAM.md            # 上游 commit + 偏离清单 + 验证记录（GPL-3.0 §5(a) 声明）
```

## 客户端

`clients/python/` 是一个纯标准库的参考实现，既是"客户端怎么对接"的活文档，也是端到端
测试的驱动器：

```bash
export FQ_API_TOKEN=your-token
python clients/python/fqapi_client.py search 剑来
python clients/python/fqapi_client.py directory <book_id>
```

```python
from fqapi_client import FqapiClient

api = FqapiClient("http://127.0.0.1:18921", "your-token")
api.search("剑来")["data"]          # 上游 JSON 原样返回
api.media_url("/src/img_abc.jpg")   # 自动带上 ?token=
```

移植自己的前端时，`backend_transport.dart` 里换成 HTTP 传输 + base URL 即可，业务代码
不用动——路径和响应形状都保持上游原样。

## 验证

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                      # 225 passed
python scripts/check_api_doc.py             # docs/API.md 与源码一致
python clients/python/e2e.py                # 真二进制 + 模拟上游 + 客户端
```

`check_api_doc.py` 拿 `router.rs` 的路径字面量、各模块的 `get_str` 与 `BadRequest`
消息当基准，双向比对文档——既能抓编造的路由/参数，也能抓漏写的。加
`--base-url http://127.0.0.1:18921 --token <token>` 还会逐条探测文档里的路径，
确认它们在真实服务上不是 404。

CI（`.github/workflows/ci.yml`）跑五道门槛：fmt+clippy、cargo test、文档契约+客户端
单测、端到端、容器构建+冒烟。只改文档不会触发——需要时从 Actions 页面手动
`Run workflow`。

## 部署前必须知道

- 默认的视频路径会**在服务端缓存解密后的整集**（`<data-dir>/src`，512 MiB 上限）。
  如果不想让服务端持有明文内容，让客户端改用 `?mode=stream`。
- 只有文件日志（`<data-dir>/rust.log`），stdout 结构化日志在 Phase 4。
- 单实例设计。多实例会各自注册设备，上游看到 N 倍注册量，更容易触发风控。
- 池里 5–20 个身份共享同一个出口 IP，比单设备更容易被风控识别。适合个人或小团队。

## 文档导航

- 上游怎么工作的 → `docs/backend-analysis.md`
- **接口怎么调** → `docs/API.md`
- 怎么移植、接口怎么设计 → `docs/PORTING-ANALYSIS.md`
- 改了上游哪些地方、怎么验证的 → `docs/UPSTREAM.md`

## 许可

GPL-3.0。本项目是 ch6vip/fqapp 的衍生作品，详见 `NOTICE` 与 `LICENSE`。

与内容提供方无隶属关系，不提供、不存放任何内容。
