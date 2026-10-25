# API 参考

服务端把番茄小说 / 短剧 / 漫画 / 听书的上游接口包装成 46 个端点、57 条路径。
**响应是上游 JSON 原样透传**——字段名、层级、`code` 字段都不改写，服务端只做三件事：
注入签名、解密正文、把媒体落盘成 `/src/...`。

## 通用约定

| 项 | 约定 |
|---|---|
| Base URL | `http://<host>:18921`（默认只监听 127.0.0.1） |
| 鉴权 | `Authorization: Bearer <token>` |
| 参数 | 一律走 **query string**；POST 路由也从 query 读参数（上游如此），body 只有账号采集用 |
| 未知参数 | 忽略。只传你需要的 |
| 成功 | `200` + 上游 JSON。业务错误也常是 `200` + `code != 0`，要看 body |
| 参数错误 | `400` + `{"success":false,"error":"..."}` |
| 未鉴权 | `401` 同上信封 |
| 路径不存在 | `404` 同上信封 |
| 上游失败 / 解密失败 | `500` 同上信封 |
| 超时 / 取消 | `504` / `499` |

路径里的 `{id}` 都是上游的 id（数字字符串），不是本地自增。

## 鉴权例外

| 路径 | 规则 |
|---|---|
| `/healthz`、`/readyz` | 永远开放 |
| `/src/*` | **永远**要凭证；额外接受 `?token=<token>`（`<img>`/`<video>` 发不了 header） |
| 其余 `/api/v1/*` | 要凭证；`--anonymous-read` 时只读路由可免 |

`?token=` 只在 `/src/*` 生效，打 API 路由会被拒。

## 媒体

| 路径 | 说明 |
|---|---|
| `GET /src/{file}` | 服务端解密后缓存的媒体。支持 `Range` / `HEAD`，不可满足返回 `416` |

`/api/v1/videos/{id}` 不带 `mode` 时会把整集下载并解密到 `/src/{video_id}.mp4`；
带 `?mode=stream` 则只返回 CDN 地址与 `key_hex`，服务端不落地。漫画没有透传模式，
图片一律解密后以 `/src/img_{sha256}.{ext}` 提供。

---

## 首页与推荐

| 方法 | 路径 | 必填 | 常用可选 | 说明 |
|---|---|---|---|---|
| GET | `/api/v1/recommend/homepage` | — | `tab_type`(2) `tab_index` `last_tab_index` `offset` `session_id` `client_template` | 首页推荐流。`tab_index` 必须跟着 `tab_type` 走，写死 0 会拿到错误的区块 |
| GET | `/api/v1/recommend` | — | 同上 | `homepage` 的别名 |
| GET | `/api/v1/recommend/channels` | — | `tab_type`(16) `last_tab_type`(-1) | 频道表，`data.tab_item[]` 逐条下发 |
| GET | `/api/v1/recommend/series-feed` | `cell_id` | `tab_type`(16) `offset` `client_req_type` `client_template` `cell_name` `cell_sub_id` `selected_items` `sub_selected_items` `filter_ids` | 短剧 feed。`cell_id` 来自首页响应 |
| GET | `/api/v1/series/{series_id}/related` | `series_id` | — | 相关短剧 |

## 搜索与分类

| 方法 | 路径 | 必填 | 常用可选 | 说明 |
|---|---|---|---|---|
| GET | `/api/v1/search` | `query` 或 `q` | `offset` `passback` `count` `tab_type`(1) `search_type` | 综合搜索。`tab_type`：综合 1 / 小说 3 / 短剧 11 / 漫画 8 / 听书 2 |
| GET | `/api/v1/search/fanqie` | `query` | `offset` | 换用番茄另一条搜索上游 |
| GET | `/api/v1/search/suggest` | — | — | **未实现**：路由存在但没有注册 handler，返回 `404 unsupported api type` |
| GET | `/api/v1/search/hot` | — | `offset` `scene`(10) | 热搜榜 |
| GET | `/api/v1/search/cue` | — | `cue_type` `tab_type` `tab_name` | 搜索框轮播提示 |
| GET | `/api/v1/category` | — | `category_tab`(1) | 分类首页（1 男频 / 2 女频） |

分页用 `offset` + `passback` 双游标：`passback` 缺省时回落 `offset`。

## 书籍

| 方法 | 路径 | 必填 | 常用可选 | 说明 |
|---|---|---|---|---|
| GET | `/api/v1/books/{book_id}` | — | — | 详情（legacy） |
| GET | `/api/v1/books/{book_id}/detail` | — | — | 同上 |
| GET | `/api/v1/books/{book_id}/directory` | — | `api_type` | 目录，`data.item_data_list[]` |
| GET | `/api/v1/books/{book_id}/directory/novel` | — | — | 图文目录（另一上游） |
| GET | `/api/v1/books/{book_id}/directory/fanqie` | — | — | `fanqienovel.com` 的目录 |
| GET | `/api/v1/books/{book_id}/chapters/summary` | `item_ids` | — | 本章说。**章节 id 走 query 的 `item_ids`，不在路径里** |
| GET | `/api/v1/books/{book_id}/reviews` | — | `count` `offset` `fold_type` | 书评 |
| GET | `/api/v1/books/{book_id}/comments` | — | `count` `offset` | 段评聚合（legacy） |
| GET | `/api/v1/books/{book_id}/related` | — | — | 相关推荐 |
| GET | `/api/v1/books/{book_id}/tones` | — | — | 音色列表 |
| GET | `/api/v1/books/{book_id}/share` | — | — | 分享信息 |
| GET | `/api/v1/novels/{post_id}` | — | — | 帖子 / 长书评详情 |

## 正文

| 方法 | 路径 | 必填 | 常用可选 | 说明 |
|---|---|---|---|---|
| GET | `/api/v1/chapters/{item_id}` | — | — | 单章正文，AES-128-CBC 解密，`data.content` 是明文 |
| GET | `/api/v1/chapters/batch` | `item_ids` | — | 批量正文，逗号分隔 |
| GET | `/api/v1/chapters/full` | `book_id` `item_ids` | — | CM/DH 握手解密，单次最多 3000 章 |
| GET | `/api/v1/chapters/{item_id}/novel` | — | — | 图文正文，另一组 DH |
| GET | `/api/v1/chapters/{item_id}/toutiao` | — | — | 头条源正文 |
| GET | `/api/v1/chapters/{item_id}/reviews` | — | `count` `offset` | 章评 |
| GET | `/api/v1/chapters/{item_id}/paragraphs/{n}/reviews` | — | `count` `offset` | 第 n 段段评 |
| GET | `/api/v1/chapters/{item_id}/paragraphs/{n}/short-reviews` | — | 同上 | 段评（短） |
| GET | `/api/v1/chapters/{item_id}/timeline` | — | — | 听书字幕时间轴 |

正文有三条解密链，服务端自动选：`chapters/{id}` → 失败回落 `chapters/full` → 图文走
`chapters/{id}/novel`。调用方不需要关心，但要知道 `full` 更慢（要握手）。

## 短剧

| 方法 | 路径 | 必填 | 常用可选 | 说明 |
|---|---|---|---|---|
| GET | `/api/v1/videos/{video_id}` | — | `mode` | 播放源。`mode=stream` 返回 `{video_url, key_hex, variants[]}` |
| GET | `/api/v1/videos/{video_id}/detail` | — | — | 剧集详情 |
| GET | `/api/v1/videos/{video_id}/danmaku` | — | — | 弹幕 |
| POST | `/api/v1/videos/{video_id}/danmaku/add` | `text` `series_id` | — | 发弹幕（写，需令牌） |
| GET | `/api/v1/series/{series_id}` | — | — | 剧集详情（series 形式，与 `videos/{id}/detail` 同一上游） |
| GET | `/api/v1/series/{series_id}/comments` | — | `count` `offset` | 剧评 |
| POST | `/api/v1/series/{series_id}/comments/add` | `text` | — | 发剧评（写，需令牌） |
| GET | `/api/v1/series/{series_id}/comments/{comment_id}/replies` | — | — | 回复列表 |
| GET | `/api/v1/series/{series_id}/hot-comments` | — | `vid` | 热评与右栏计数 |
| GET | `/api/v1/series/{series_id}/share` | — | — | 分享 |

`variants[]` 是高→低的清晰度列表，空数组表示单流（客户端应隐藏清晰度入口）。
编码门在服务端：`bytevc2` 会被拒，所以列表里不会出现它。

## 评论

| 方法 | 路径 | 必填 | 常用可选 | 说明 |
|---|---|---|---|---|
| GET | `/api/v1/comments/{comment_id}/replies` | — | `count` `offset` | 回复列表 |
| POST | `/api/v1/comments/{comment_id}/digg` | — | `series_id` | 点赞（写，需令牌） |
| POST | `/api/v1/comments/{comment_id}/reply` | `text` `reply_to_comment_id` | `series_id` `group_id` `book_id` | 回复（写，需令牌） |
| GET | `/api/v1/comments/short-url` | `target` | — | 分享短链解析 |

三条 POST 路由是**设备绑定写路由**：它们以本机的设备身份提交动作，所以即使开了
`--anonymous-read` 也必须带令牌。

## 漫画

| 方法 | 路径 | 必填 | 常用可选 | 说明 |
|---|---|---|---|---|
| GET | `/api/v1/manga/{item_ids}` | — | `api_type` `book_id` `show_html` | 漫画章节。返回 `images: ["/src/img_<sha256>.jpg", ...]` |
| GET | `/api/v1/manga/videos/{pseries_id}` | — | `offset`(1) `count`(30) `total`(80) `pseries_type`(5) | 漫剧剧集列表 |

`show_html=1` 时额外返回一段可直接渲染的 `<img>` HTML。

## 听书

| 方法 | 路径 | 必填 | 常用可选 | 说明 |
|---|---|---|---|---|
| GET | `/api/v1/audio/books/{book_id}` | — | — | 有声书详情 |
| GET | `/api/v1/audio/books/{book_id}/chapters/{chapter_id}` | — | — | 章节播放信息 |
| GET | `/api/v1/audio/play` | `item_ids` `book_id` | `tone_id` | 播放源 |
| GET | `/api/v1/audio/wkcontent/{item_ids}` | — | `genre` `tone_id` | 文稿 |

## 作者与榜单

| 方法 | 路径 | 必填 | 常用可选 | 说明 |
|---|---|---|---|---|
| GET | `/api/v1/authors/{author_id}` | — | — | 作者信息 |
| GET | `/api/v1/authors/{author_id}/bookshelf` | — | `count` `offset` | 作者作品 |
| GET | `/api/v1/rank/{rank_id}` | — | `algo_type` `genre_tab` `offset` `count` `rank_sub_info_id` | 榜单数据 |

## 其它

| 方法 | 路径 | 必填 | 常用可选 | 说明 |
|---|---|---|---|---|
| GET | `/api/v1/items/{item_ids}` | — | — | 章节信息（不含正文） |
| GET | `/api/v1/articles/{item_ids}` | — | — | **未实现**：路由存在但没有注册 handler，返回 `404 unsupported api type` |
| GET | `/api/v1/forum-id` | `author_user_id` `book_id` `item_id` | — | 圈子 id |

## 账号采集（默认不对外推荐）

| 方法 | 路径 | 必填 | 说明 |
|---|---|---|---|
| POST | `/api/v1/account/fanqie/library` | body: `cookie` `resource` | 读用户番茄书架。9 个固定只读 RPC，URL/方法/路径全部白名单 |
| POST | `/api/v1/account/fanqie/history` | body: `cookie` `resource` | 读阅读历史 |

凭证只允许出现在 **POST body**（≤16 KiB），不会进 URL、日志或设备池；请求不跟随重定向。
这两个端点转发的是用户自己的账号 Cookie，务必只在自己掌控的部署里开放。

## 健康检查

| 路径 | 语义 |
|---|---|
| `GET /healthz` | 进程活着。恒 `200`，带 `{"status":"ok","devices":N}` |
| `GET /readyz` | 池里至少 1 个可用设备才 `200`，否则 `503 {"status":"starting","devices":0}` |

冷启动要注册 5 个设备（每个含 2 秒停顿），约 20 秒。就绪探针请用 `/readyz`，
存活探针用 `/healthz`——就绪失败就重启容器是错的。
