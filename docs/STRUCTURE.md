# 代码结构

一个业务一个目录，目录内按职责分文件。文件树与 Rust 模块树一致，`mod.rs` 是模块入口；小模块可以在入口内实现，复杂实现拆到同目录的文件或子目录。

```text
src/
├── lib.rs              # 插件注册
├── plugin/             # 宿主装配、生命周期、帮助与状态命令
├── auth/               # 平台登录命令、二维码与数据库私有凭据
│   ├── bilibili/       # B 站官方扫码协议、会话、凭据校验
│   ├── music/          # 音乐平台共用的扫码任务、回执、取消和凭据模型
│   ├── ncm/            # 网易云客户端二维码协议
│   ├── qqmusic/        # QQ 音乐 QQ 扫码与 OAuth 授权协议
│   ├── sessions.rs    # 串行化各平台账号变更，保留其它平台会话
│   ├── store.rs        # 多平台共用的 Nuono 私有 secret 适配器
│   └── qr_image.rs     # 有界的黑白二维码生成
├── config/             # 默认声明、运行配置类型与加载
│   ├── default_config.rs # 配置结构体及默认值、说明、约束、网页控件
│   └── model.rs        # 配置加载、媒体目录选择
├── runtime/            # 消息处理流程、准入规则、冷却、统计和运行状态
├── resolver/           # 链接识别、平台路由、解析结果契约
│   ├── manager.rs      # 装配与路由
│   ├── adapter.rs      # 平台接口与分享页公共流程
│   ├── bilibili/       # B 站链接、API、数据映射、评论及媒体候选
│   ├── douyin/         # 抖音 API、分享页、公开统计和评论
│   │   ├── video.rs    # 匿名移动 Feed 主备请求与作品 ID 校验
│   │   ├── playback.rs # 无平台水印播放源、高清请求及候选排序
│   │   └── desktop/    # 页面元数据、DOM 索引、评论及统计补充
│   ├── kuaishou/
│   ├── kurobbs/
│   ├── ncm/
│   ├── qqmusic/        # 官方歌曲详情、Vkey、短链和播放权限
│   ├── qq_channel/
│   ├── twitter/
│   └── xiaohongshu/    # PC/移动 HTTP 分享页与公开笔记状态
│       ├── state.rs    # 内联状态解析及笔记 ID 绑定
│       └── video.rs    # 原视频 key、MediaV2 和网页播放源候选
├── render/             # 素材准备、图像处理、文字与卡片渲染
│   ├── comments.rs     # 海报与图集共用的评论、配图和表情
│   ├── icons.rs        # 共用统计与评论点赞图标
│   ├── plain/          # 图集下方的统计与评论布局
│   └── poster/         # 视频与歌曲共用的封面海报布局
├── download/           # 队列预约、图片与媒体下载、缩略图、FFmpeg 合流
│   ├── images.rs       # 渲染素材与原图投递共用的有界图片获取
│   ├── gallery.rs      # 原图抓取及 SDK 图片编码
│   └── task/           # 后台调度及图文先发、媒体后发的顺序控制
├── delivery/           # 消息构造、回复目标、发送回执
└── live/               # 直播订阅命令、存储、轮询和开播通知
```

`assets/` 保存内嵌字体和图标；`tests/fixtures/` 保存离线页面素材。

## 修改位置

- 增加或修复平台解析：修改 `resolver/<平台>/`；新增平台时在 `platform.rs` 声明平台，在 `manager.rs` 装配适配器。
- 调整解析结果里的封面、作者或评论：修改平台映射；共享数据结构位于 `resolver/card.rs`、`content.rs` 和 `media.rs`。
- 调整卡片样式：修改 `render/plain.rs` 或 `render/poster/`；图集统计位于 `render/plain/engagement.rs`，评论和统计图标位于 `render/comments.rs` 与 `render/icons.rs`；字体与 Emoji 位于 `render/text.rs`，图像解码和尺寸限制位于 `render/image.rs`。
- 原链接二维码位于 `render/link_qr.rs`，本地编码并保留完整分享参数。海报使用融入背景的浅色图案，普通卡片用深色图案，文案为“扫码查看原作品”。优先使用用户发来的原始链接，保留四模块空白区、整数模块和高纠错；过长或无效链接只省略二维码，不影响卡片。
- 调整视频下载：修改 `download/manager.rs`；发送顺序和后台取消位于 `download/task/`。
- 调整登录：修改 `auth/<平台>/`；`auth/commands.rs` 注册仅超管私聊命令，`auth/store.rs` 通过 SDK 私有 secret 接口访问宿主数据库，不新建业务表。B 站 Cookie 和刷新凭据整体存入 `plugin.LinkShowNN.bili_qr_session`，不进入配置文件、配置表单或日志；数据库及备份包含明文凭据。成功保存后替换解析器，取消或失败保留旧账号，清除恢复手动 Cookie 或匿名访问；刷新凭据当前不自动续期。
- 网易云、QQ 音乐扫码凭据分别放在 `ncm_qr_session`、`qqmusic_qr_session` 私有 secret 中，登录任务共用 `auth/music/`，平台协议不持有宿主状态。`auth/sessions.rs` 用同一把变更锁串行执行候选解析器构建、单平台存储及完整会话发布；B 站登录、清除也通过此入口，避免覆盖其它平台账号。重启读取三平台凭据，停用取消所有扫码任务。
- 调整默认配置或网页配置项：修改 `config/default_config.rs`。结构体用 `#[derive(nuono_plugin::ConfigSchema)]`，字段通过 `#[config(...)]` 同时声明默认值、分组、控件、选项和条件显示；SDK 负责生成 schema、补全默认值和解码列表，宿主通过 `Plugin::config_fields()` 生成表单。网页默认跟随字段声明顺序，无需手写 `order`；配置按通用配置、媒体下载、内容过滤、B站、抖音、小红书、网易云音乐和 Twitter/X 连续分组，B站直播订阅也放在 B站组。QQ 音乐当前没有独立配置项，禁用入口在通用配置，扫码登录凭据由宿主数据库保存。`model.rs` 只保留加载和目录选择，`mod.rs` 只负责模块导出。

平台模块负责平台数据及媒体候选，共享下载器负责执行下载；消息流程负责协调解析、渲染和回复，渲染布局只接收卡片数据和已取得的素材。跨业务使用明确的入口和类型，生产模块使用显式导入；测试可以直接访问所属模块的内部实现。

平台解析只使用 HTTP/API 和静态分享页，不执行页面 JavaScript；卡片由 Rust 原生绘图生成。分享页可读取 HTML 中已有的内联状态，接口和静态页面没有取得作品数据时报告失败。插件不启用 SDK 的浏览器引擎。

网易云默认直连官方歌曲详情与 eapi 播放接口，`ncm_api_host` 默认留空；旧配置中的自部署地址仅在原生歌曲信息获取失败时作为备用，数据库中的 Cookie 不转发到该地址。预览依据返回的 `freeTrialInfo` 或真实播放时长判断，并在卡片及文件名中标记；不能仅凭 `fee` 推断是否完整。QQ 音乐请求官方详情与对应歌曲的 Vkey，仅接受匹配歌曲与文件的已授权播放 URL，不把试听专用地址或测试音频当作完整歌曲。

歌曲卡片与视频共用 `render/poster/` 的封面渐变背景、白字、圆角和原作品二维码。`CardPoster.cover_url` 指定专辑封面，`video_cover_url` 只标识视频；歌曲不伪装成视频或图集。歌曲详情保留专辑、时长、音质及试听/授权提示，没有歌手头像时显示文字占位，封面缺失则回退普通图文。

音乐二维码只内存发送，网易云使用音乐 App 扫码，QQ 音乐使用 QQ App 扫码授权。需要会员的歌曲以平台对该账号返回的权限为准；验证码、失效凭据或额外购买要求不会通过其它来源补造。命令为 `url网易云登录`、`urlQQ音乐登录` 及对应的“登录状态”“取消登录”“清除登录”。协议参考 [media-parser 音乐解析](https://github.com/ucmao/media-parser/tree/main/src/parsers)、[网易云维护者接口](https://github.com/neteasecloudmusicapienhanced/api-enhanced)、[QQMusicApi](https://github.com/L-1124/QQMusicApi)。

图集走纯文字与原图投递：1–3 张原图同消息发送，超过 3 张在 OneBot 群聊或私聊使用合并转发，最多 8 张；其它平台按其图文发送能力回退。独立封面、头像和评论配图不计入原图数量。发送确认仍先于视频，转发失败或未知回执不会重复投递或放行后续视频。`delivery/gallery.rs` 管文字及节点，`delivery/scoped.rs` 管停用取消。

测试跟随所属业务，较大的测试集放在 `tests.rs`。内嵌素材通过 `CARGO_MANIFEST_DIR` 引用，目录移动不影响字体、图标和页面 fixture 的路径。

## 验证

在插件目录执行：

```bash
cargo test --locked --lib
cargo test --locked --test http_smoke
```

第二条使用本机 HTTP fixture 验证分享页请求、重定向、平台域名与 Cookie 边界，要求能够监听临时回环端口；它不访问实际平台或数据库。需要网络和外部环境的预览测试保持显式忽略。

## 抖音能力边界

抖音解析不启动外部浏览器，只读取接口与公开静态分享页。完整路由数据或经过作品 ID 验证的 DOM 才能生成回复；验证页、推荐作品与通用站点封面不冒充解析成功。原图、统计和评论仍按真实返回的数据映射，缺失时不补造。

普通视频优先请求移动 Feed 主备接口，只带作品 ID、`aid=1128` 和应用 User-Agent，不发送登录 Cookie 或请求签名；Feed 同时会返回推荐作品，必须匹配请求的作品 ID。图集优先使用公开 `slidesinfo` 接口。两种接口失败后再尝试网页详情与静态分享页；评论总数不代表取得评论正文。移动 Feed 请求参考 [media-parser 的实现](https://github.com/ucmao/media-parser/blob/main/src/parsers/douyin_parser.py)。

抖音下载优先用 API 返回的真实视频 URI 请求官方 `play` 端点，参数为 `watermark=0`，`ratio` 随 `douyin_quality` 选择，备用为同一作品的播放源；只对已知官方播放端点规范化，不改 CDN 直链签名。请求档位不代表实际分辨率，鸣潮样本的 720P/1080P 已验证有效，360P/480P 请求仍得到1080P。主源实际尺寸未知时卡片写“请求NNNP”，不伪装为已确认画质。下载层最多尝试三个源，共用时间预算；超过配置大小限制后停止，不通过换源绕过限制。

小红书先读取桌面分享页的 H.264 播放源，失败后尝试移动分享页及原视频字段；保留 `xsec_token` 等分享参数，并严格选择 URL 对应的笔记。`originVideoKey`、MediaV2 和多个 CDN 源沿用平台签名，不承诺每个作品匿名可读。原始上传源可能使用 HEVC，桌面 H.264 有效时优先直接下载以避免转码；已知的带水印流不作为下载候选。B 站直接下载平台下发的 DASH 或单文件播放源，DASH 合流使用 `-c copy`，已有画面标志不会被去除。

配置入口仍集中在 `config/default_config.rs`：`show_comments` 默认开启，关闭后所有平台卡片及图集文字隐藏评论素材，作品评论数统计保留；B站关闭额外评论请求，小红书不再为有效电脑端内容额外抓移动评论页。`bili_quality`（默认720P）、`douyin_quality`、`xhs_quality`（默认1080P）分别选择360P/480P/720P/1080P；平台缺档时按实际源回退，原始源无尺寸时不猜档位。B站按需通过匿名 HTML5 补充缺少的720P，只采用完整、合法且更高清的单文件流，失败保留有效DASH；未登录提示只描述本次获取结果，不推断所有作品统一的匿名上限。

小红书 `xiaohongshu/engagement.rs` 从笔记的 `interactInfo` 读取点赞、收藏、评论和分享统计，再通过移动端 HTTP 分享页补充 `commentData.comments`，不使用登录或外部浏览器。两份页面都必须匹配请求笔记 ID；移动端失败时保留电脑端结果，额外请求最多等待 6 秒。可见根评论按 `likeViewCount`（缺失时才用 `likeCount`）排序、去重、取三条，不查询受限分页接口。头像、`pictures[].url` 和 `emotes.json` 中的官方表情素材共用现有渲染资源预算，素材失效时保留文字。图集继续纯文字与原图投递。

匿名卡片实测（只在显式运行时访问平台，不读取本机凭据、不发送消息）：

```bash
NUONO_XHS_LIVE_OUTPUT=/tmp/linkshownn-xhs-preview cargo test --lib public_xiaohongshu_live_preview -- --ignored --nocapture
```

匿名媒体探测在独立测试进程运行，不读取数据库或登录凭据：

```bash
# 显式访问一个公开作品，输出是否取得经过验证的作品信息
cargo test --test http_smoke -- --media-probe https://www.douyin.com/video/7693165612066999571 http
# 显式验证实际下载，最多 32 MiB；样本保存在临时目录，不发送消息
cargo test --test http_smoke -- --media-probe https://v.douyin.com/B4nEt6lLK2w/ http-download
# B 站或小红书的匿名 HTTP 验证；不读取运行配置或保存账号 Cookie
cargo test --test http_smoke -- --media-probe https://www.bilibili.com/video/BV1jJhU6kETw http-download
```

探测不会执行页面 JavaScript、发送聊天消息或保存平台 Cookie。可用 `NUONO_MEDIA_PROBE_PROXY` 显式设置代理。
