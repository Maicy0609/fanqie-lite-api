# 上游同步记录

本项目的 `crates/fqapi-core` 移植自 **ch6vip/fqapp** 的 Rust 核心。本文登记上游版本、
同步方式，以及相对上游的**每一处**偏离，供日后比对与升级。

> GPL-3.0 第 5(a) 条要求修改过的文件带有显著声明。本文即该声明。

---

## 1. 上游基线

| 项 | 值 |
|---|---|
| 仓库 | https://github.com/ch6vip/fqapp |
| 分支 / commit | `master` @ `4eaeac3` |
| 提交总数 | 496 |
| 上游路径 | `rust/` |
| 许可证 | GPL-3.0（`LICENSE` 已原样保留） |
| 上游版权 | Copyright (C) 2026 ch6vip |

本项目的 `crates/fqapi-core` 与上游 `rust/` 的对应关系：

```
fqapp/rust/src/          →  crates/fqapi-core/src/
fqapp/rust/tests/        →  crates/fqapi-core/tests/      (逐字节一致)
fqapp/rust/testdata/     →  crates/fqapi-core/testdata/   (逐字节一致)
fqapp/rust/Cargo.toml    →  crates/fqapi-core/Cargo.toml  (已裁剪，见 §3)
```

## 2. 同步方式

上游没有提供可用的 crate 依赖（`publish = false`），因此采用**源码复制 + 差异登记**，
不做 `git subtree`（上游仓库包含整个 Flutter 应用，subtree 会把无关内容带进来）。

升级上游时：

```bash
# 1. 取上游新版本到独立目录（不要直接覆盖）
git clone https://github.com/ch6vip/fqapp /tmp/fqapp-new

# 2. 对比差异，确认新增/变更的文件
diff -rq /tmp/fqapp-new/rust/src crates/fqapi-core/src

# 3. 逐个文件搬运，跳过本文 §3 登记的本项目改动
# 4. 必须重新跑：cargo test（tests/ 与 testdata/ 同步更新后仍应全绿）
```

**判断搬对了的标准**：`cargo test` 全绿，且 `tests/`、`testdata/` 与上游逐字节一致。
任何需要修改测试才能通过的情况，都说明搬错了。

---

## 3. 相对上游的偏离（Phase 0）

Phase 0 的目标是"骨架，不改行为"。因此偏离只有"删掉不存在的传输层"这一类。

### 3.1 删除的文件

| 文件 | 上游行数 | 原因 |
|---|---:|---|
| `src/frb_generated.rs` | ~2,000 | flutter_rust_bridge 生成代码，服务端无 FFI |
| `src/desktop_media.rs` | 481 | Windows libmpv 桌面播放器专用（`#[cfg(windows)]`） |
| `build.rs` | 27 | 仅为 `desktop_media` 编译 C 解密核心（`cc` 构建依赖） |

### 3.2 重命名的模块

| 上游 | 本项目 | 说明 |
|---|---|---|
| `src/api/mod.rs` | `src/runtime/mod.rs` | 模块名不再指代一个不存在的 FFI 桥。**逻辑零改动**：3 个 hunk 共 61 行差异，除日志安装门禁外全是注释 |

### 3.3 修改的文件

| 阶段 | 文件 | 改动 | 行为影响 |
|---|---|---|---|
| 0 | `src/lib.rs` | 删 `mod frb_generated;`、`#[cfg(target_os = "windows")] mod desktop_media;`；`pub mod api` → `pub mod runtime`；文档注释改写 | 无 |
| 0 | `src/runtime/mod.rs` | ① 模块文档改写；② `set_device_pool_key` 文档改写（同时修掉上游"Backend core version."的错位文档）；③ `init` 中 `core_log::install` 去掉 `#[cfg(target_os = "android")]` 门禁 | **有**：非 Android 构建现在也会写 `rust.log` |
| 1 | `src/runtime/mod.rs` | ④ 新增 `pub async fn server() -> Option<Arc<Server>>`（9 行，含文档）。`request()` 会把响应体读进内存且上限 64 MiB，`/src/*` 媒体必须流式 | 无（只暴露既有字段） |
| 2 | `src/runtime/mod.rs` | ⑤ `init_core` 由 `pub(crate)` 改为 `pub`，并按 `ListenMode::Disabled` 跳过监听；⑥ 日志安装从 `init` 移进 `init_core`（否则直接调 `init_core` 的调用方拿不到任何日志） | 无（`init` 行为不变） |
| 2 | `src/server.rs` | 新增 `ListenMode::Disabled` 变体；`serve()` 对该变体返回错误（正常路径不会到达） | 无 |
| 2 | `src/device/mod.rs` | 新增 `pub async fn persist()`（3 行）：显式触发一次 `save`，用于密钥刚就绪时立刻把明文池迁移成加密 | 无（只是把 `save` 暴露出来） |
| 0 | `src/core_log.rs` | 删 `android_logger` 分支：`LOGCAT_TAG` 常量、`CoreLogger.android` 字段、`new()` 中的初始化、`log()` 中的镜像、`flush()` 中的转发；文档注释同步 | **有**：不再镜像到 logcat（服务端没有 logcat） |
| 4 | `src/core_log.rs` | `log()` 写完文件后把同一行 `println!` 到 stdout（3 行） | **有**：容器只采集 stdout，否则 `docker logs` 里看不到任何核心诊断 |
| 0 | `Cargo.toml` | 删 `flutter_rust_bridge`、`android_logger`、`[build-dependencies] cc`、`[target.'cfg(windows)'.dependencies] reqwest/blocking`；`crate-type` 改为 `["rlib"]` | 无 |
| 0 | 根 `Cargo.toml` | 新增 workspace；`[profile.*]` 从成员清单移到根（Cargo 只认根 profile） | 无 |

### 3.4 零改动的文件（逐字节一致）

```
src/config.rs          src/dispatch.rs       src/error.rs
src/crypto.rs          src/filter.rs         src/json.rs
src/static_files.rs    src/timeutil.rs
src/endpoints/**       src/sign/**           src/upstream/**
tests/**               testdata/**
```

Phase 0 时 47 个源文件逐字节一致；Phase 2 之后 `server.rs` 与 `device/mod.rs` 各有一处
小改动，降为 45 个。`endpoints/`（约 12,000 行）与 `sign/`（约 1,300 行）始终未动。

---

## 4. Phase 0 验证记录

执行时间：2026-10-25

| 检查 | 命令 | 结果 |
|---|---|---|
| 严格编译 | `cargo check --all-targets` | 通过，零警告 |
| 严格 lint | `cargo clippy --all-targets -- -D warnings` | 通过 |
| 全量测试 | `cargo test --workspace` | **225 passed / 0 failed**（core 182 + server 43） |
| 测试资产一致性 | 见下方脚本 | 19 个文件逐字节一致 |
| 未改动源码一致性 | 见下方脚本 | 45 个文件逐字节一致 |
| 真实上游端到端 | 见 §4.2 | 搜索、目录均返回真实数据 |
| 加密与监听 | 见 §4.3 | 明文池启动即加密；只有一个监听端口 |
| 媒体鉴权与 Range | 见 §4.4 | 401 / 200 / 206 / 416 全部符合预期 |
| 镜像构建与运行 | 见 §4.5 | 139 MB，非 root，HEALTHCHECK healthy，日志在 stdout |

一致性检查必须比对**上游仓库内容**，不能比对上游工作树：Windows 上
`core.autocrlf=true` 会把上游 checkout 成 CRLF，直接 `diff -r` 会得到满屏假差异。
本项目用 `.gitattributes` 把 `*.rs` 与 `testdata/**` 钉死为 LF，工作树、索引、仓库三者
一致。

```bash
cd crates/fqapi-core
# 测试资产：应输出 19 个文件、无 DIFF
for f in $(find tests testdata -type f | sort); do
  git -C /path/to/fqapp show "HEAD:rust/$f" | cmp -s - "$f" || echo "DIFF: $f"
done

# 未改动源码：应输出 45 个文件、无 DIFF
for f in $(find src -name '*.rs' | sort); do
  rel=${f#src/}
  case "$rel" in runtime/mod.rs|core_log.rs|lib.rs|server.rs|device/mod.rs) continue;; esac
  git -C /path/to/fqapp show "HEAD:rust/src/$rel" | cmp -s - "$f" || echo "DIFF: $rel"
done
```

测试分布：

```
fqapi_core  unittests (src/lib.rs)   80
            crypto_vectors           13
            device_pool               9
            device_pool_encrypted     1
            device_pool_no_key        1
            filter_config             4
            loopback_guard           13
            manga_end_to_end          3
            mock_upstream_contract    7
            playlet_interaction      23
            router_contract           2
            static_and_loopback      17
            video_codec_gate          9
fqapi_server unittests (lib)         25
             unittests (main)         2
             tests/http.rs           16
```

### 4.2 真实上游端到端（Phase 1 验收）

2026-10-25，本机 Windows，`--data-dir target/e2e --token <t> --listen 127.0.0.1:19021`：

| 请求 | 结果 |
|---|---|
| `GET /healthz`（无 token） | `200 {"devices":0,"status":"ok"}` |
| `GET /readyz`（池空） | `503 {"status":"starting","devices":0}` |
| `GET /api/v1/search?query=剑来`（无 token） | `401 {"success":false,...}` |
| `GET /api/v1/search?query=剑来`（带 token） | `200`，返回真实搜索结果（含 `book_data`） |
| `GET /readyz`（注册后） | `200 {"devices":5,"status":"ok"}` |
| `GET /api/v1/books/<id>/directory`（带 token） | `200`，659 KB JSON |

冷启动耗时约 20 秒（5 个设备 × 注册流程内的 2 秒停顿），期间 `readyz` 一直是 503 ——
这正是 `/readyz` 存在的原因。

**本次验证暴露的缺口**：池文件当时以**明文**写入（`rust.log` 有
`saving device pool unencrypted: no pool key provisioned` 警告），里面是 5 个真实设备的
`secret_key`。这是 Phase 2 的第一项，在完成前**不适合部署到任何真实环境**。

### 4.3 Phase 2 端到端（离线，不访问上游）

2026-10-25，用一个预置了 5 个**明文**设备的池启动服务端：

| 检查 | 结果 |
|---|---|
| `pool.key` 自动生成 | 64 个 hex 字符 |
| 启动后池文件头部 | `FQPOOL1`（明文已被迁移为加密） |
| 该进程的监听端口 | 只有 `--listen` 指定的一个（隐藏的 loopback 适配器已不再启动） |
| `rust.log` | `core init: port=0 mode=Disabled` |
| 明文池的告警 | 无（密钥已就绪，不再走明文分支） |
| `GET /readyz` | `200 {"devices":5}` |
| 删掉 `pool.key` 再启动 | 退出码 1，提示 "encrypted but no key is available…" |

最后一项是刻意设计的：密钥缺失时**拒绝启动**，而不是悄悄生成新密钥。新密钥打不开旧池，
池会以空状态加载，然后重新注册 5 个设备——旧身份就永久丢了。

### 4.4 Phase 3 媒体（真实二进制 + 真实 socket）

`--data-dir target/e2e4`（预置 5 个设备 + `src/clip.mp4` 16 字节），`--token media-token`：

| 请求 | 结果 |
|---|---|
| `GET /src/clip.mp4`（无凭证） | `401` |
| `GET /src/clip.mp4?token=media-token` | `200`，`content-type: video/mp4`、`accept-ranges: bytes`、`referrer-policy: no-referrer`、`cache-control: private, max-age=3600` |
| `Range: bytes=0-3` | `206`，`content-range: bytes 0-3/16`，body `0123` |
| `Range: bytes=4-7` | `206`，body `4567` |
| `Range: bytes=99-` | `416` |
| `GET /api/v1/search?query=x&token=media-token` | `401`（query 凭证仅限媒体） |

**Phase 3 没有改动任何 core 文件。** 媒体服务、Range/416、缓存预算（512 MiB，LRU）
上游已经实现并且有测试（`video.rs` 的 `prune_src_cache_with_budget` 单测、`static_files`
的 Range 测试）。这一阶段只补了两件 server 侧的事：媒体路径的凭证策略，以及响应头加固。

### 4.5 Phase 4 镜像与日志

2026-10-25，Docker 29.9.2（linux/amd64），`docker build -t fqapi-lite-api:local .`：

| 检查 | 结果 |
|---|---|
| 构建耗时 | 2 分 27 秒（release 编译 1 分 02 秒，LTO + codegen-units=1） |
| 镜像大小 | 139 MB |
| 运行身份 | `uid=10001(fqapi)`，非 root |
| 合规文件 | `/usr/share/doc/fqapi-server/{LICENSE,NOTICE}` 均在 |
| `docker ps` | `Up (healthy)` —— HEALTHCHECK 打 `/healthz` 通过 |
| `scripts/smoke.sh` | 6 项全过 |

`scripts/smoke.sh` 的实际输出（容器内真实完成设备注册与上游调用）：

```
ok   /healthz 200 without a token
ok   /api/v1/search 401 without a token
ok   /api/v1/search 200 with a token
ok   /src/* 401 without a token
ok   ?token= does not open API routes
info /readyz 200 (200 = the device pool registered upstream)
```

`docker logs` 的实际输出，确认两条出口都通、且**没有记录 query string**：

```
[I] 2026-10-25T03:21:43 core: fqapi_core::runtime: core init: port=0 mode=Disabled config=/data/config/config.json
fqapi-server listening on http://0.0.0.0:18921
[I] 2026-10-25T03:21:48 access: GET /healthz 200 0ms
[I] 2026-10-25T03:21:57 access: GET /api/v1/search 401 0ms
[I] 2026-10-25T03:21:59 access: GET /api/v1/search 200 2211ms
```

最后一行 2211 ms 是首次调用触发的设备注册；同一进程内后续请求回到毫秒级。

**构建时踩到的两个环境问题**（不是项目问题，记录备查）：

1. `docker` 与 `docker-credential-desktop` 都不在默认 PATH（本机装在
   `F:\Tools\Docker\resources\bin`），后者缺失会让 `docker build` 在解析
   `# syntax=docker/dockerfile:1` 时报 "error getting credentials"。
2. `scripts/smoke.sh` 里 `export MSYS_NO_PATHCONV=1`：Git Bash 会把 `-v ...:/data`
   里的 `/data` 改写成 Windows 路径，容器内就找不到挂载点。

---

## 5. 待办偏离（Phase 5 及以后）

按 `docs/PORTING-ANALYSIS.md` 的计划，以下改动尚未实施。每实施一项，**必须**在 §3 追加记录。

| # | 内容 | 阶段 | 状态 |
|---|---|---|---|
| 1 | `device/mod.rs::get()` / `get_by_id()` 的写盘改为脏标记 + 后台 flush | — | **不做（撤回）**，理由见下 |
| 2 | 公开监听层（鉴权 / CORS / 限流 / 探针） | Phase 1 | **已完成**，见 `crates/fqapi-server`（未改 `server.rs` 的既有行为） |
| 3 | 设备池 key 来源改为 `<data-dir>/pool.key` | Phase 2 | **已完成**，见 §4.3 |
| 4 | 媒体代理模式（`--media-proxy`）与签名 URL + TTL | Phase 3 | **不做（重新定义）**，理由见下 |
| 5 | `core_log` 增加 stdout 输出 | Phase 4 | **已完成**，见 §3.3 |
| 6 | `error.rs` 增加可选 `code` 字段 | 延后 | 暂无消费方，避免加入未被使用的抽象 |
| 7 | `video.rs::handle_pseries` 的 `eprintln!` 改为 `log::warn!` | — | **不做**：上游那样写是因为 Android 没有控制台；服务端 stderr 本来就会被采集 |
| 8 | 让初始化支持不启动 loopback 适配器 | Phase 2 | **已完成**（`ListenMode::Disabled`），见 §4.3 |

**为什么撤回第 1 项。** 我最初把它列为"必修"，理由是"每个请求写一次盘"。复核后这个
判断站不住：

- 池文件 5–20 个设备约 5 KB，`save()` 没有 `fsync`，只是一次页缓存写 + rename；
- 每个请求真正的耗时是上游 HTTP 往返（几十到几百毫秒），写盘占比不到 1%；
- 改它必须修改上游测试 `concurrent_gets_do_not_corrupt_the_pool_file`（它断言
  `get()` 之后文件里的 `use_count >= 1`），而"测试零改动"是判断移植是否忠实的主要依据。

用扩大偏离面 + 改上游测试，去换一个在真实负载下测不出来的收益，不划算。如果将来压测
证明它是瓶颈，再按脏标记 + 后台 flush 重做，并同步更新该测试。

**为什么第 4 项改成"不做"。** 计划里写的是"新增 `/media/v/{id}`、`/media/img/{hash}`，
URL 带 HMAC 签名和 24 小时 TTL"。落地时发现两件事：

1. **代理已经存在。** `/src/*` 就是代理：核心把解密后的视频与漫画图写进 `src_dir`，
   `static_files::serve` 带 Range/HEAD/416 提供出去。再加一套 `/media/*` 只是同一份
   能力的第二个名字，属于"为了形式上的单一职责制造包装层"。
2. **签名 URL 需要改写响应，而改写会打断现有客户端。** 客户端靠前缀识别媒体
   （`fqapp/lib/services/episode_source_cache.dart` 的 `_extractVideoUrl` 只认
   `http(s)://` 与 `/src/`）。把响应里的 `/src/` 换成 `/media/...?s=...`，客户端会
   直接丢掉这个 URL。要做得对，得同时改客户端——那不是移植期该做的事。

所以采用最小方案：`/src/*` 额外接受 `?token=` 查询参数（`<img>`/`<video>` 发不了
header），并给媒体响应加 `Referrer-Policy: no-referrer` 与 `Cache-Control: private`。
代价是 token 可能出现在浏览器历史或代理日志里；这是明确的取舍，不是疏忽。

**将来真需要"把媒体链接交给第三方"时**再做签名 URL，届时的前提是客户端能接受
`/media/*` 前缀——那是一次协议变更，必须和客户端一起改。

**明确不做**（登记为技术债，不在移植期清理）：

- `reading724_params` 的四份拷贝（`home.rs` / `comments.rs` / `audio.rs` / `book.rs`）
- `HOST_FQNOVEL` 与 `HOST_FQNOVELB` 值相同
- `config.admin` 字段从未被使用
- `filter.rs` 的空壳实现（`enabled=true` 即启动失败）
- `version_code=66.9` 等刻意的非法值

理由见 `docs/PORTING-ANALYSIS.md` §4.2 / §10：移植期混入重构，出问题时无法区分
"搬错了"还是"改坏了"。

---

## 6. 上游换密钥时需要改的常量

上游一旦轮换签名材料，请求会稳定 403。此时按下表逐项核对：

| # | 位置 | 常量 | 用途 |
|---|---|---|---|
| 1 | `sign/argus.rs` | `ARGUS_SIGN_KEY_HEX` | x-argus 的 Simon 密钥（32B） |
| 2 | `sign/argus.rs` | `ARGUS_AES_KEY_HEX` / `ARGUS_AES_IV_HEX` | x-argus 外层 AES-128-CBC |
| 3 | `sign/argus.rs` | bean 内 `f1 = 1077940818`、`f8 = "v04.04.05-ov-android"`、`f9 = 134744640`、`f23 = {NX551J, 8196, 0x80E0D800}` | bean 版本指纹 |
| 4 | `sign/mod.rs` | `Manager::new()` 的 `aid = 1967`、`license_id = 1_611_921_764` | 应用标识 |
| 5 | `device/crypto.rs` | `REGISTER_KEY_MASTER_HEX` | registerkey 响应解密主密钥 |
| 6 | `device/crypto.rs` | `TT_FIXED_STRING_B64` / `TT_MAGIC` | device_register 请求加密 |
| 7 | `device/crypto.rs` | `REGISTER_KEY_REQ_KEY_B64` | registerkey 请求加密 |
| 8 | `device/register.rs` | `SDK_VERSION`、`PKG_NAME` | 注册指纹 |

验证方式：`rust/testdata/vectors.json` 与 `crypto_vectors.rs` 是独立参考实现产出的
黄金向量。换常量后先跑 `cargo test --test crypto_vectors`，确认签名算法本身没被改坏，
再用真实上游请求验证。
