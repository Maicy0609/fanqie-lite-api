# fqapp 后端逻辑分析

> 分析对象：`E:\0\fqapp`（什锦铺，Flutter + Rust 混合应用，GPL-3.0，master @ `4eaeac3`，496 次提交）
> 文档来源：README / DEVELOPMENT（48 KB）/ WINDOWS / CONTRIBUTING / native/README / scripts/README / installer / patches / branding / tiny_aes
> 代码来源：`rust/`（18,566 行 src，46 条路由）、`native/`（CENC 解密 C 库）、`lib/services/`（Dart 侧调用面）

---

## 0. 一句话结论

**这个项目没有服务端后端。**

它的"后端"是编译进 APK 的 Rust 静态库 `libfqapi_core.so`（crate `fqapi_core`），跑在 Flutter 同一进程内，通过 flutter_rust_bridge 的 FFI 被 Dart 直接调用；同一个分发器另开一个 `127.0.0.1:18921` 的 loopback HTTP 端口，把 `/api/v1/*` 只读路由和 `/src/*` 媒体文件开放给外部阅读器（Legado 书源等）。

所以所谓"后端逻辑"= **四件事**：

| 职责 | 实质 |
|---|---|
| 身份 | 在本地维持 5–20 个已注册的番茄设备身份（device_id / install_id / secret_key） |
| 签名 | 为每个上游请求伪造 `x-argus` / `x-ladon` / `x-ss-stub` 等风控头 |
| 解密 | AES-128-CBC、CM/DH 大素数握手、AES-256-GCM、CENC AES-CTR 四条解密链 |
| 归一化 | 把上游五花八门的 JSON 原样透传，只在必要处改写 `content` 字段 |

---

## 1. 分层结构

`lib.rs` 的注释就是权威分层，实际代码与之一致：

```
api       (920 行)  flutter_rust_bridge 门面：init / shutdown / request / cancel
dispatch  (288 行)  唯一分发器，FFI 与 loopback HTTP 共用
endpoints(~12k 行)  路由表 + 46 个业务 handler（manga 3162 / video 2613 / comments 957 / book 597 ...）
upstream  (~800 行) 带签名的 HTTP 客户端、设备轮换、CM/DH 密钥交换
device    (~1700 行) 设备池、四步注册、池文件加密持久化
sign      (~1300 行) argus / ladon / gorgon / simon / sm3 / protobuf
crypto    (314 行)  AES-CBC、PKCS7、gzip（带炸弹上限）
server    (332 行)  axum loopback 适配器 + capability 鉴权
static_files (236)  /src/* 受限静态文件（Range / HEAD / 416）
```

依赖选型很克制：`reqwest 0.13`（rustls，**关掉默认特性**，只链一个 TLS provider）、`axum 0.8`（只有 http1/tokio/query）、`aes` + `cbc`、`num-bigint`（手写 DH/CM）、`flate2`、`zeroize`。没有 `aes-gcm`，GCM 是手写的；没有 JS 引擎，所以 `filter.json` 一旦 enabled 就直接启动失败。

`[profile.release]` 刻意设了 `opt-level="s" + lto + strip`，但**没有** `panic="abort"` —— 注释写明：FRB 把 panic 转成 Dart 侧的类型化错误，abort 会直接杀掉整个 App。

---

## 2. 生命周期与并发模型

### 2.1 启动

```
main.dart → BackendService.start()
  → 部署 config.json / filter.json / device_pool.example.json → files/backend/config/
  → 首次启动用 example 初始化 device_pool.json
  → SecureKeyProvider().devicePoolKey()  → rust.setDevicePoolKey(32B)
  → rust.init(config, pool, filter, runtimeDir, port=18921, lanShare, mockOrigin='')
      → INIT_LOCK 串行化
      → Config::load → resolve_runtime_dir → mkdir src/
      → DevicePool::new（读池、解密、损坏备份）
      → Manager::new()（aid=1967, license=1611921764, argus 551 风格, gorgon off）
      → UpstreamClient::new（reqwest + rustls + webpki-roots，15s 超时，max_retries=3）
      → FilterManager::load_with_base
      → Server::new(ctx)  ← register_all 挂 46 条路由
      → server::serve（绑定 127.0.0.1 或 0.0.0.0，生成 32B 随机 capability）
      → GENERATION.fetch_add(1) → 发布到全局 CORE
  → 轮询 /health 直到 200（总 15s，失败退避 ≤300ms）
```

关键设计：`INIT_LOCK` 让 8 个并发 `init` 都返回 `"running"` 而不是 7 个 `address already in use`（有专门的回归测试）；`GENERATION` 单调递增，保证旧核心的清理不会误删新核心的在途请求。

### 2.2 请求取消与超时

`request(request_id, method, path, query, body, timeout_ms)` 是唯一入口：

- 每个请求注册到全局 `INFLIGHT: HashMap<id, (generation, CancellationToken)>`
- `tokio::select!`（`biased`）竞争三件事：取消信号 / deadline / 实际工作
- **取消 = drop 掉 dispatch future**，连带丢弃 await 中的 reqwest 请求、重试退避、尚未提交的写
- 取消返回 `499 {"success":false,"error":"cancelled"}`，超时返回 `504 ... "timeout"`
- `PRE_CANCELLED` 有界队列（256）记录"先取消后到达"的 id
- `InflightRegistration` 用 `Drop` 清理，panic 也不会泄漏登记

`shutdown()` 的顺序值得注意：先 abort listener task → `pool.retire().await`（等 commit gate，保证已在 rename 的写盘完成、之后的写全部拒绝）→ 再 cancel 所有在途 token。这是"关机不能返回时还有旧写能落盘"的强保证。

---

## 3. 请求处理全链路

```
UI 页面
  → ApiClient（Dart，1732 行）
      · RequestGate：相同并发 GET 合并成一次上游飞行，全局最多 16 并发
      · Zone 传递 BackendRequest 做取消
      · _decodeAsync：后台 isolate 解 JSON，避免大 payload 卡 UI
  → BackendTransport
      ├─ RustBackendTransport：rust.request(...)  ← 生产路径，不过 socket
      └─ HttpBackendTransport：http://127.0.0.1:18921/_session/<cap>/...  ← 测试/桌面/Web
  → dispatch::dispatch
      1. /api/v1/account/fanqie/history|library  → 特殊 POST 处理（凭证只走 body）
      2. /health                                  → {status, devices}
      3. /src/*                                   → static_files::serve（Range/HEAD）
      4. router::match_rest_path                  → 命中 46 条之一，否则 anti_crawler 302 或 404
      5. 写路由方法校验（非 POST → 405 + Allow）
      6. raw_handler（HTML 页，绕过 JSON 信封）
      7. router::pin_from_session                 → session_id 绑定的设备 pin 注入
      8. handler(ctx, params)
           → Upstream::raw_with_device / json
               → substitute_device({device_id}/{install_id}/{secret_key}/{cdid}/{openudid})
               → Manager::generate_headers(query, body)
                   x-ss-req-ticket / x-khronos / x-ladon / x-argus / x-ss-stub [/ x-gorgon]
               → HttpTransport::request_with_limit
                   reqwest(rustls) → 流式读 body（超 64 MiB 立即失败）→ gzip 解压（超 64 MiB 硬失败）
           → 失败分类：401/403/10001/10002/网络错误 = DeviceFailed → 换设备重试（最多 3 次）
           → 解密 / 改写 content
      9. ctx.filters.apply(path, data)             ← 当前恒等（filter.json 全 disabled）
     10. Response::json(200, data) 或 {success:false,error}
```

错误只有两类：`BadRequest → 400`、`Internal → 500`，信封统一 `{"success":false,"error":"..."}`。客户端同时接受上游的 `code=200`（Web 桥接）和 `code=0`（上游兼容）作为成功。

---

## 4. 签名体系（最核心的逆向资产）

`sign/mod.rs::Manager::generate_headers_at` 是唯一装配点：

| Header | 生成方式 |
|---|---|
| `x-ss-req-ticket` | `ts * 1000` |
| `x-khronos` | `ts`（秒） |
| `x-ladon` | `ladon_encrypt(ts, license_id, aid, random4)` —— MD5 展开 288 字节哈希表 + 自定义分组加密 |
| `x-argus` | 见下 |
| `x-ss-stub` | `hex(MD5(body))`，空 body 不生成 |
| `x-gorgon` | 仅注册流程启用（`enable_gorgon = true`），常规请求关闭 |

### x-argus（551 风格）

```
ProtoBuf bean:
  f1=1077940818  f2=2  f3=rand_i32  f4=aid  f5=device_id  f6=license_id
  f7=version_name(query)  f8="v04.04.05-ov-android"  f9=134744640
  f10=8×0x00  f11=0  f12=ts<<1
  f13=SM3(stub_bytes)[0..6]   ← 空 stub 用 SM3(16×0x00)
  f14=SM3(query_bytes)[0..6]  ← 空 query 用 SM3(16×0x00)
  f15={1,1,1,0xC792ECCC}  f16=install_id  f20="none"  f21=738
  f23={"NX551J",8196,0x80E0D800}  f25=2
→ PKCS7 对齐
→ Simon(128/128, 固定 32B 密钥) 逐块加密
→ 前后各拼 8B 魔数 f2f7fcff... / 9B 尾 ea2b...  → XOR 首 8 字节 → 整体反转
→ 追加 'a','o' → PKCS7 → AES-128-CBC(固定 key/iv)
→ 前置 [0xf2, 0x81] → Base64
```

硬编码常量：`ARGUS_SIGN_KEY_HEX`（Simon 密钥）、`ARGUS_AES_KEY_HEX` / `ARGUS_AES_IV_HEX`。这三组值是这个项目能工作的根本，也是它最脆弱的地方。

### 一个容易踩的坑

签名输入是 **query string 和 body 的 MD5**，而设备占位符替换发生在签名之前（`do_with_device_inner` 先 `substitute_device` 再 `do_resp` 签名）。所以任何"先签名后替换"或"漏替换"的实现都会稳定 403。代码里有三处注释专门记录这个坑（fanqie 搜索、novel 目录、pseries）。

---

## 5. 设备池（身份供给）

`MIN_POOL_SIZE = 5`、`MAX_POOL_SIZE = 20`，LRU（按 `last_used` 字符串排序），`use_count` 计数。

### 5.1 四步注册

```
new_dev_info()：随机 openudid(16 hex) / cdid / uuid / clientudid / rom / UA
  1. POST https://log.snssdk.com/service/2/device_register/
       body = gzip({magic_tag:"ss_app_log", header:{约 40 个设备指纹字段}, _gen_time})
              → tt_encrypt: magic(6B) + random32 + AES-128-CBC(PKCS7(SHA512(data)+data))
              key/iv = SHA512(SHA512(random32) + 固定 Base64 串)[0:16]/[16:32]
       → device_id / install_id
  2. GET  https://ichannel.snssdk.com/service/2/app_alert_check/     （失败忽略）
  3. sleep 2s
  4. POST https://api5-normal-sinfonlinea.fqnovel.com/reading/user/privilege/add/v/  （失败忽略）
  5. POST https://reading.snssdk.com/reading/crypt/registerkey
       body = gzip({content: base64(iv + AES-128-CBC(hex2bin(reverseHex(device_id))))})
       签名启用 x-gorgon；最多 3 次重试（每次 1s）
       → data.key → decrypt_register_key（固定 master key ac25c67d...）
       → secret_key = hex::encode_upper(明文)
```

`registerKey` 失败时**仍然返回 Device**，只是 `secret_key` 为空。这是刻意的降级，但会污染池子 —— 所以 `get()` / `refill()` / `replace_failed()` 三处都显式拒绝空 key 设备（"registration success ≠ usable device"）。

### 5.2 失败替换

`DeviceFailed`（401/403/10001/10002/网络错误/本地 `TooLarge` **不算**）→ `pool.replace_failed(device_id)`：最多 5 次注册尝试，退避 1..5s，成功则原位替换并持久化，持久化失败则回滚内存状态。

### 5.3 持久化

`device_pool.json` 是 `{android:[Device...], last_update}`。有 pool key 时整文件 AES 加密（key 来自 Android Keystore，Dart 侧 `SecureKeyProvider` 提供），无 key 时明文 + 一次性 warn。

写入路径的并发正确性做得相当细：

- `version` 快照 + `CommitGate` 锁：写盘和 rename 在锁内完成，`retire()` 取同一把锁
- 临时文件名带 `instance + tmp_seq`，避免两个池实例互相 rename 对方的 tmp
- 已加密文件在无 key 保存前会先 `rename` 成 `.corrupt-<ts>` 备份，绝不用明文覆盖密文
- 解析失败的文件同样备份而不是静默丢弃

### 5.4 会话绑定

首页推荐的 `session_id` 绑定到当时应答的设备（10 分钟 TTL，上限 10000 条，过期与最旧双淘汰）。翻页时 `pin_from_session` 注入 `_pinned_device`，保证第 N+1 页不会换设备导致推荐流断裂。

---

## 6. 四条解密链

| 场景 | 算法 | 密钥来源 | 位置 |
|---|---|---|---|
| 小说 / 漫画正文（batch、full/v） | AES-128-CBC，`base64(iv‖ct)`，可选 gzip | `device.secret_key`（hex→16B） | `crypto::decrypt_upstream` |
| `full` 接口（novelfm mget） | CM 握手：3072-bit MODP（RFC3526 g15）DH，shared secret 前 32B 作 AES-256 key | 客户端临时私钥 + 服务端 `key` 字段 | `upstream/cm.rs` |
| 小说图文正文（reader content v1） | 另一组 DH（十进制 1024 位素数），响应头 `y` + `c` | 请求头 `y` 携带的临时公钥 | `upstream/dh.rs` |
| 漫画图片 | AES-256-GCM（12B nonce + 16B tag），手写 GCM | 正文 JSON 的 `encrypt_key`（hex，32B） | `manga.rs::decrypt_manga_image` |
| 短剧 / 漫剧视频 | CENC AES-CTR | `video_model` 的 spade 派生 key | `native/`（Android）+ `video.rs`（Rust 实现/桌面） |

两个安全细节：CM/DH 都对对端公钥做了**范围 + 小群校验**（`2 ≤ y ≤ p-2` 且 `y^q == 1`）；PKCS7 在自有协议路径用严格版（校验每个填充字节），只有 `decrypt_upstream` 为兼容上游保留宽松版。

---

## 7. 路由与上游映射

46 条路由，12 个模块：

| 域 | 路由 | 上游 |
|---|---|---|
| 首页 | `recommend/homepage` `recommend/channels` `recommend/series-feed` `series/{id}/related` | `bookmall/tab/v`、`bookmall/cell/change/v`、`plan/v` |
| 搜索 | `search` `search/suggest` `search/hot` `search/cue` `category` | `reading/bookapi/search/tab/v`、`search/cue/v`、`plan/v`、`new_category/front/v` |
| 详情 | `books/{id}` `books/{id}/detail` `novels/{id}` `items/{id}` `authors/{id}` `authors/{id}/bookshelf` | `reading/bookapi/detail/v`、`ugc/postdata/detail/v`、`ugc/item/mix_data/get/v` |
| 目录 | `books/{id}/directory` `books/{id}/directory/novel` `books/{id}/directory/fanqie` | `directory/all_items/v`、`novel/book/directory/list/v1`、`fanqienovel.com/api/reader/directory/detail` |
| 正文 | `chapters/{id}` `chapters/{id}/novel` `chapters/batch` `chapters/full` | `reader/batch_full/v`、`reader/full/v`、`novelfm/playerapi/full/mget/v1` |
| 评论 | `books/{id}/reviews` `books/{id}/comments` `chapters/{id}/reviews` `chapters/{id}/paragraphs/{n}/reviews` `comments/{id}/replies` `chapters/{id}/timeline` | `commentapi/*`、`ugc/novel_comment/*`、`idea/list` |
| 短剧 | `videos/{id}` `videos/{id}/detail` `series/{id}` `series/{id}/comments` `series/{id}/hot-comments` `series/{id}/share` `videos/{id}/danmaku` | `player/video_model/v1`、`player/multi_video_model/v1`、`player/video_detail/v1` |
| 漫画 | `manga/{id}` `manga/videos/{id}` | 复用 full/batch + 图片下载 |
| 听书 | `audio/books/{id}` `audio/books/{id}/chapters/{cid}` `audio/play` `audio/wkcontent/{id}` `chapters/{id}/timeline` | `bookapi/audio/*`、`novel/player/*` |
| 榜单 | `rank/{id}` `books/{id}/tones` | `rank`、`audio/tone` |
| 分享 | `books/{id}/share` `series/{id}/share` `comments/short-url` `resolve` | `share/info/v`、`share/short_url` |
| 账号 | `POST account/fanqie/history` `POST account/fanqie/library` | 9 个固定白名单 RPC |

上游域名共 11 个：`api5-normal-sinfonlinea/b`、`api3-normal-sinfonlinea/b`、`novel.snssdk.com`、`reading.snssdk.com`、`fanqienovel.com`、`log.snssdk.com`、`ichannel.snssdk.com`、`novelfm-hl.snssdk.com`、`api.fanqiesdk.com`。

**账号采集是唯一"带凭证"的路径**：Cookie 只允许出现在 POST body（≤16 KiB），`deny_unknown_fields` 严格反序列化，URL/方法/路径全部来自硬编码白名单，禁止重定向（`HttpTransport::without_redirects`），响应上限 8 MiB，且不进入设备重试。

---

## 8. 四条关键业务链

### 8.1 首页推荐

`reading724Params` 是一份约 90 个字段的固定参数包（分辨率、电量、屏幕亮度、`network_type=4`、`cold_start_session=1`…），每次请求再叠加 `tab_type` / `tab_index` / `offset` / `session_id`。

两个已被真机验证的坑：

- `tab_index` 不能写死 `"0"`，否则任何分类都回推荐流的区块（"听书"里出现推荐榜）
- `recommend/channels` 必须 `bottom_tab_type=7` + `client_req_type=3`，否则拿到的是书城频道条而不是短剧频道条

### 8.2 搜索

`/api/v1/search` 按 `tab_type` 分流：综合 1 / 小说 3 / 短剧 11 / 漫画 8 / 听书 2。分页用 `offset` + `passback` 双游标，`passback` 缺省时回落 offset。漫剧没有独立 tab，复用 11 后按 `genre=205` 在客户端二次过滤。

### 8.3 阅读（三级降级）

```
1. /chapters/{id} 或 /chapters/batch     → AES-128-CBC（快，单请求）
2. 失败 → /chapters/full                 → CM DH 握手（慢，但 key 独立）
3. 图文 → /chapters/{id}/novel           → 另一组 DH（响应头 y/c）
```

`full` 解密失败时**主动删除 `content` 字段**而不是留着密文 —— 因为 `content.rs` 只检查 `content` 非空，留着 base64 会把密文当正文返回给用户。这个 bug 有专门的单测守着。

### 8.4 播放（最复杂的一条）

```
mode=stream:
  tokio::join!(
    shortplay video_model  (h265 各档),
    phoenix multi_video_model (含 1080P，但低档是解不了的 bytevc2)
  )
  → 两路合并清晰度（单靠任一路都会缺档）
  → resolve_video_source 逐档尝试，编码门：h265/h264 可播，bytevc2 拒绝
  → 选中档 → spade 解密 URL（SPADE_CONSTANTS 64 字节表 + 自定义字节变换）
  → content_key（16B）以 hex 返回
  → Android: ExoPlayer + libshortplay_crypto.so 按 Range 边读边 CENC 解密
     Windows: libmpv + desktop_media.rs
非 stream:
  → 下载整集到 src/，本地解密 CENC（Rust 实现 MP4 box 解析），返回本地路径
  → 缓存预算 512 MiB，单文件上限 2 GiB，LRU 淘汰
```

`manga` 的处理类似但更简单：解密正文 → 拿 `picInfos` + `encrypt_key` → 逐图下载 → AES-256-GCM 解密 → 按内容哈希命名写 `src/` → 返回 `/src/img_<sha256>.<ext>`。内容寻址是为了避免不同章节同名图片互相覆盖（有过真实 bug）。

---

## 9. 安全边界

| 面 | 措施 |
|---|---|
| Loopback 监听 | 默认只绑 `127.0.0.1`；`lan_share` 才绑 `0.0.0.0` |
| DNS rebinding | `Host` 必须是 `127.0.0.1` / `localhost` / （Lan 模式下的）IP 字面量；域名一律拒绝 |
| CSRF | 带 `Origin` 时必须同源；`Origin: null` 视为外部 |
| Capability | 32B 随机 token，可通过 `/_session/<cap>` 前缀、`Bearer`、`fq_session_<port>` cookie 三种方式出示 |
| 写路由 | 只有 3 条（评论 add / digg / reply）强制 capability，防止同机其他 App 冒名发评论 |
| 静态文件 | 单次 percent-decode → 拒 `..` / `:` / NUL → canonicalize → 必须落在 root 内 → 只服务普通文件 |
| 请求体 | loopback 64 MiB（流式丢弃，不缓冲）；账号采集 16 KiB |
| TLS | 只用 `webpki-roots`，账号采集禁重定向 |
| 设备池 | Android Keystore 派生 key 加密落盘；无 key 时明文 + warn |
| 密钥内存 | DH/CM 私钥用 `Zeroizing`，临时缓冲 `write_volatile` 擦除 |

---

## 10. 观测与测试

**日志**：`rust.log`（1 MiB 轮转，1 份备份）+ logcat tag `fqapi_core`。之所以要落盘，是因为 Android 应用读不到自己的 logcat（`READ_LOGS` 是系统权限）。格式与 Dart 侧 `app.log` 同形，共用解析器。

**测试**：Rust 侧 3,859 行集成测试 + 各模块内嵌单测，覆盖：

- `crypto_vectors.rs`：argus/ladon/sm3/simon 的黄金向量（由独立参考实现生成，不是本 crate 自产）
- `device_pool*.rs`：池容量、加密/无密钥、损坏备份、并发 rename、retire 竞态
- `loopback_guard.rs` / `static_and_loopback.rs`：Host/Origin/capability/Range/路径逃逸
- `mock_upstream_contract.rs`：本地 mock 上游跑真实签名请求（离线可测）
- `router_contract.rs`：46 条路由的路径匹配契约
- `video_codec_gate.rs` / `manga_end_to_end.rs`：编码门与图片解密端到端

`rust/testdata/` 里 6 份夹具（`vectors.json`、`device_vectors.json`、`dh_content_fixture.json`、`manga_image_fixture.json`、`cenc_sample_fixture.json`、`series_related.json`）随仓库分发，构建不依赖私有环境。

---

## 11. 设计上的取舍与遗留问题

**做得好的地方**

1. 单一分发器 —— FFI 和 HTTP 走完全相同的代码，不存在"两套逻辑漂移"
2. 取消语义是"drop future"，而不是设标志位，连带释放连接和退避
3. 设备池的写盘并发模型（version + gate + retire）考虑到了进程重启的边界
4. 失败分类清晰：`TooLarge` 不当作设备故障，避免因响应过大而轮换身份
5. 注释密度极高，且大量注释记录的是"为什么"，包括真机复现路径和反编译出处

**值得注意的问题**

1. **硬编码指纹散落各处** —— UA、`version_code`、`device_type`、`resolution`、`rom_version` 在每个 endpoint 里各写一遍，上游一改就要全仓库搜。`content.rs` 里 `version_code=66.9` 这种非法数值是刻意对齐上游 Go 实现的，不能"修正"。
2. **密钥全在二进制里** —— argus 的 Simon key / AES key / IV、registerkey master key 都是明文常量。这是逆向工程的必然结果，但也意味着上游换 key 就必须重新逆向。
3. **`filter.json` 是空壳** —— 一旦有路由 `enabled=true`，启动直接报错（"no JavaScript runtime"）。JS 过滤器能力实际上没实现。
4. **`video.rs::handle_pseries` 里残留 `eprintln!`** —— 绕过了 `log` 宏，release 版不会进 `rust.log`，排障时看不到。
5. **`reading724_params` 有三份拷贝**（home / comments / audio / book），字段一旦不同步就会出现"某个页面参数不对"的诡异问题。
6. **上游脆弱性** —— README 自己承认"这个 App 没有自己的服务器，没法绕过上游的变化"。任何上游改协议，都只能发新版。

---

## 12. 如果要复刻成独立 API 服务（对 `fanqie-lite-api` 的启示）

**必须原样搬走的四块**

| 块 | 文件 | 不可替代性 |
|---|---|---|
| 签名 | `sign/{argus,ladon,gorgon,simon,sm3,protobuf}.rs` | 没有它，任何请求都 403 |
| 设备 | `device/{mod,register,crypto}.rs` | 没有 secret_key，正文全是密文 |
| 解密 | `crypto.rs` + `upstream/{cm,dh}.rs` | 三条链缺一条就少一类内容 |
| 端点参数模板 | `endpoints/*.rs` 里的 URL 与 90 字段参数包 | 参数缺一个就可能降级或空结果 |

**可以砍掉的**

`api/mod.rs`（FRB 门面）、`server.rs`（loopback）、`static_files.rs`、`filter.rs`、`core_log.rs`、`desktop_media.rs`。独立服务直接用 axum/hyper 暴露路由即可，`dispatch::dispatch` 的签名（`Request → Response`）本身就是天然的 HTTP handler 抽象。

**改造顺序建议**

1. 先做 `sign` + `device` + `upstream`，用 `/health` + 一个搜索接口打通"能拿到上游 JSON"
2. 再接 `content`（AES-128）→ 验证 `secret_key` 正确
3. 再接 `full`（CM DH）→ 验证大素数握手
4. 最后接 `video`（spade + CENC）和 `manga`（GCM），这两个最耗时

**五个必须注意的坑**

1. 占位符替换必须在签名之前，否则 403
2. 设备轮换要保留 `session_id` pin，否则推荐流翻页断裂
3. 注册流程的 `sleep 2s` 和 gorgon 签名不能省，否则 `registerKey` 拿不到 key
4. `TooLarge` / 解析失败不能触发设备轮换，否则会白白烧掉池子
5. 并发注册必须串行（`register_lock`），否则一次冷启动会打出十几个注册请求

**合规提醒**

项目自身的产品边界写得很清楚：不做登录、不做云同步、不写上游、不做"解锁"类功能，只读消费公开内容。复刻时如果越过这条线（比如回写账号、绕过付费），性质和这个项目就不同了。
