# fanqie-lite-api

把 [fqapp](https://github.com/ch6vip/fqapp) 的后端（Rust 核心）移植为**独立 HTTP 服务**，
让前端可以多平台复用、前后端分离、并接入第三方工具。

当前状态：**Phase 0 完成**（骨架移植，业务逻辑零改动，测试全绿）。

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
docs/
  CODING.md              # 实现原则
  backend-analysis.md    # 上游后端逻辑分析
  PORTING-ANALYSIS.md    # 移植方案、接口设计、阶段计划
  UPSTREAM.md            # 上游 commit + 偏离清单（GPL-3.0 §5(a) 声明）
```

## 验证

```bash
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test            # 182 passed
```

## 文档导航

- 想知道上游怎么工作的 → `docs/backend-analysis.md`
- 想知道怎么移植、接口怎么设计 → `docs/PORTING-ANALYSIS.md`
- 想知道改了上游哪些地方 → `docs/UPSTREAM.md`

## 许可

GPL-3.0。本项目是 ch6vip/fqapp 的衍生作品，详见 `NOTICE` 与 `LICENSE`。

与内容提供方无隶属关系，不提供、不存放任何内容。
