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

| 文件 | 改动 | 行为影响 |
|---|---|---|
| `src/lib.rs` | 删 `mod frb_generated;`、`#[cfg(target_os = "windows")] mod desktop_media;`；`pub mod api` → `pub mod runtime`；文档注释改写 | 无 |
| `src/runtime/mod.rs` | ① 模块文档改写；② `set_device_pool_key` 文档改写（同时修掉上游"Backend core version."的错位文档）；③ `init` 中 `core_log::install` 去掉 `#[cfg(target_os = "android")]` 门禁 | **有**：非 Android 构建现在也会写 `rust.log`。这是服务端必需的行为 |
| `src/core_log.rs` | 删 `android_logger` 分支：`LOGCAT_TAG` 常量、`CoreLogger.android` 字段、`new()` 中的初始化、`log()` 中的镜像、`flush()` 中的转发；文档注释同步 | **有**：不再镜像到 logcat（服务端没有 logcat） |
| `Cargo.toml` | 删 `flutter_rust_bridge`、`android_logger`、`[build-dependencies] cc`、`[target.'cfg(windows)'.dependencies] reqwest/blocking`；`crate-type` 改为 `["rlib"]` | 无 |
| 根 `Cargo.toml` | 新增 workspace；`[profile.*]` 从成员清单移到根（Cargo 只认根 profile） | 无 |

### 3.4 零改动的文件（逐字节一致）

```
src/config.rs          src/dispatch.rs       src/error.rs
src/crypto.rs          src/filter.rs         src/json.rs
src/server.rs          src/static_files.rs   src/timeutil.rs
src/device/**          src/endpoints/**      src/sign/**
src/upstream/**        tests/**              testdata/**
```

合计 18,085 行中的 17,600+ 行未经修改。

---

## 4. Phase 0 验证记录

执行时间：2026-10-25

| 检查 | 命令 | 结果 |
|---|---|---|
| 严格编译 | `cargo check --all-targets` | 通过，零警告 |
| 严格 lint | `cargo clippy --all-targets -- -D warnings` | 通过 |
| 全量测试 | `cargo test` | **182 passed / 0 failed** |
| 测试资产一致性 | 见下方脚本 | 19 个文件逐字节一致 |
| 未改动源码一致性 | 见下方脚本 | 47 个文件逐字节一致 |

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

# 未改动源码：应输出 47 个文件、无 DIFF
for f in $(find src -name '*.rs' | sort); do
  rel=${f#src/}
  case "$rel" in runtime/mod.rs|core_log.rs|lib.rs) continue;; esac
  git -C /path/to/fqapp show "HEAD:rust/src/$rel" | cmp -s - "$f" || echo "DIFF: $rel"
done
```

测试分布：

```
unittests (src/lib.rs)              80
crypto_vectors                      13
device_pool                          9
device_pool_encrypted                1
device_pool_no_key                   1
filter_config                        4
loopback_guard                      13
manga_end_to_end                     3
mock_upstream_contract               7
playlet_interaction_contract        23
router_contract                      2
static_and_loopback                 17
video_codec_gate                     9
```

---

## 5. 待办偏离（Phase 1 及以后）

按 `docs/PORTING-ANALYSIS.md` 的计划，以下改动尚未实施。每实施一项，**必须**在 §3 追加记录。

| # | 内容 | 阶段 | 性质 |
|---|---|---|---|
| 1 | `device/mod.rs::get()` / `get_by_id()` 的写盘改为脏标记 + 后台 flush | Phase 2 | 行为变更（落盘时机） |
| 2 | `server.rs` 替换为公开监听（鉴权 / CORS / 限流） | Phase 1 | 行为变更 |
| 3 | 设备池 key 来源改为 `<data-dir>/pool.key` | Phase 2 | 行为变更 |
| 4 | 媒体代理模式（可选，`--media-proxy`） | Phase 3 | 新增能力 |
| 5 | `core_log` 增加 stdout 输出 | Phase 4 | 新增能力 |
| 6 | `error.rs` 增加可选 `code` 字段 | Phase 1 | 向后兼容的新增 |
| 7 | `video.rs::handle_pseries` 的 5 处 `eprintln!` 改为 `log::warn!` | Phase 1 | 修复（上游缺陷） |

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
