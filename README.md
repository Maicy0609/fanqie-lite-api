# fanqie-lite-api

把 [fqapp](https://github.com/ch6vip/fqapp) 的后端（Rust 核心）移植为**独立 HTTP 服务**，
让前端可以多平台复用、前后端分离、并接入第三方工具。

当前状态：**Phase 1 完成**（骨架移植 + HTTP 服务壳）。业务代码 95% 原样搬自上游。

## 快速开始

```bash
cargo run -p fqapi-server -- --token change-me --data-dir ./data
```

```bash
curl http://127.0.0.1:18921/healthz
curl -H "Authorization: Bearer change-me" \
     "http://127.0.0.1:18921/api/v1/search?query=%E5%89%91%E6%9D%A5"
```

首次启动会在后台注册 5 个上游设备身份，约 20 秒；期间 `/readyz` 返回 503。
没有 `--token` 会直接拒绝启动 —— 一个不鉴权的监听端口等于把本机的设备身份借给任何人。

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
`FQ_MAX_CONCURRENCY`、`FQ_TIMEOUT_SECS`、`FQ_CORS_ORIGINS`。

### 鉴权

所有 `/api/v1/*` 默认要求 `Authorization: Bearer <token>`。两条例外：

- `/healthz`、`/readyz` 永远开放（探针没有令牌）
- `--anonymous-read` 下只读路由开放，但**设备绑定写路由**（发评论、发弹幕、点赞）与
  `/api/v1/account/*` 仍必须带令牌

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
  src/http.rs          # axum 路由、dispatch 适配、流式文件、CORS
  src/auth.rs          # Bearer 鉴权 + 常量时间比较
  src/health.rs        # /healthz /readyz
docs/
  CODING.md              # 实现原则
  backend-analysis.md    # 上游后端逻辑分析
  PORTING-ANALYSIS.md    # 移植方案、接口设计、阶段计划
  UPSTREAM.md            # 上游 commit + 偏离清单 + 验证记录（GPL-3.0 §5(a) 声明）
```

## 验证

```bash
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test            # 209 passed
```

## 部署前必须知道

- **设备池当前是明文**（Phase 2 加 `<data-dir>/pool.key`）。里面是真实设备的
  `secret_key`，完成前不要放到任何真实环境。
- 媒体仍走上游的 `/src/*` 落盘路径（Phase 3 才做透传/代理两种模式）。
- 只有文件日志（`<data-dir>/rust.log`），stdout 结构化日志在 Phase 4。
- 单实例设计。多实例会各自注册设备，上游看到 N 倍注册量，更容易触发风控。

## 文档导航

- 上游怎么工作的 → `docs/backend-analysis.md`
- 怎么移植、接口怎么设计 → `docs/PORTING-ANALYSIS.md`
- 改了上游哪些地方、怎么验证的 → `docs/UPSTREAM.md`

## 许可

GPL-3.0。本项目是 ch6vip/fqapp 的衍生作品，详见 `NOTICE` 与 `LICENSE`。

与内容提供方无隶属关系，不提供、不存放任何内容。
