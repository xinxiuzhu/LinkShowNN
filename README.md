# LinkShowNN

[LinkShowNN](https://github.com/xinxiuzhu/LinkShowNN) is a multi-platform link preview and media plugin for [Nuono](https://github.com/xinxiuzhu/Nuono).

The project, installation directory, plugin name and Rust type use `LinkShowNN`. The Cargo package and crate use the conventional lowercase name `linkshownn`.

Supported sources include Bilibili, Douyin, Kuaishou, Xiaohongshu, Twitter/X, NetEase Cloud Music, Kurobbs, and QQ channels. The plugin is built as a Rust library linked with Nuono and has no Python runtime dependency.

## Build and install

Place this repository under `Nuono/plugins/LinkShowNN/`. Rust 1.99.0 or later is required. Run from the Nuono root:

```bash
git clone https://github.com/xinxiuzhu/LinkShowNN.git plugins/LinkShowNN
cargo plugin check plugins/LinkShowNN
cargo plugin test plugins/LinkShowNN
cargo run
```

The plugin remains an independent Cargo workspace. Nuono discovers its source and links it with the host, sharing the host `target/` directory. Source updates require a rebuild and restart. Only install trusted plugins: they share the host process and permissions. Nuono child builds follow `[build] incremental` in `nuono.toml`, with Cargo environment overrides taking priority.

## Upgrading from the previous name

For an existing `nuono-urlresolver` / `NuonoUrlResolver` installation, stop Nuono before upgrading. Keep one source installation: rename the plugin directory to `plugins/LinkShowNN` and update its source. Copy `data/NuonoUrlResolver/` to `data/LinkShowNN/`, including `.config-file-authority-v1`, without overwriting an existing destination. Keep the old data as a backup. Explicit `media_output_dir` values should remain unchanged; only the default macOS download directory changes to `~/Downloads/LinkShowNN`.

Nuono also keys global enablement and group overrides by the runtime plugin name. The new name does not automatically inherit the old database records. Review and reapply the old enablement, user-toggle and group settings for `LinkShowNN` before resuming message processing. Rebuild and restart Nuono after updating source; a Dashboard instance reload alone does not update linked code.

## Configuration

Defaults and Dashboard fields are declared in `src/default_config.rs`. User values remain in Nuono's plugin configuration file. The plugin reads a complete snapshot when it loads; after saving configuration, reload the plugin instance or restart Nuono to apply it. Configuration keys and defaults remain compatible with existing installations.

Cookie fields use password controls in Dashboard and remain ordinary file configuration. For `xhs_cookie` and `douyin_cookie`, paste a request `Cookie` header value such as `name=value; name2=value2`, not a response `Set-Cookie` value. Initial short-link expansion does not carry these cookies, including when JavaScript is needed to reach the content page. Cookies are then scoped to the corresponding platform's content domain and its subdomains, including same-domain APIs. HTTPS cookies use `Secure`; `__Host-` cookies remain host-only, and prefixed secure cookies require an HTTPS content URL.

Ordinary HTTP/API requests default to direct connections. Obscura page requests use upstream environment proxy settings when the plugin's `proxy` field is empty; an explicit plugin proxy overrides them. The plugin does not modify the process environment.

## 图文卡片

解析回复默认将作者、发布时间、正文和配图合成一张 PNG。视频使用海报版式：顶部清晰封面、模糊背景与暗色渐变、大标题、独立统计区、作者头像和身份信息；B 站展示真实发布时间、BV 号、UID、播放/评论及赞币藏转。普通图文保留白底版式，多图排成两列。缺失的信息自动略过，统计缺失不会补成零。直播订阅通知保持原有发送方式。

B 站视频封面来自官方 `view.pic`，不以 `pages[].first_frame` 替换，也不抽取视频帧。海报背景只是该封面的模糊延伸。渲染在插件 `src/render/` 中通过 Rust 绘图完成；Obscura 继续负责动态网页解析，当前没有接入 HTML/CSS 截图后端。

B 站视频海报附带热门评论：通过独立匿名客户端读取首批最多 20 条热评，去重后按点赞数降序展示最多 3 条，包含头像、昵称、正文和点赞数。这是匿名接口返回的热评候选中的前三，不保证全评论区的绝对点赞前三；不登录、不使用已配置的 B 站 Cookie，也不遍历楼中楼或继续翻页。评论接口最多等待 3 秒，取不到就省略评论区；每条正文最多显示 3 行并提示截断。关闭图文卡片时不请求评论。

评论支持正文内的普通/大号表情（包括 MyGO 等表情包）及最多 3 张附图。只替换接口明确给出素材地址的完整表情标记，按行内图片自动换行；素材失败时保留 `[表情名]`，配图失败会显示提示。输出仍为静态 PNG，动态素材只取单帧。

评论素材共用 4 秒下载时限和整张卡片的素材预算，最多并发 2 个请求：头像最多 3 张、每张 512 KiB；表情每条评论最多 8 种、全卡最多下载 16 种、每张 512 KiB；配图每条评论最多 3 张、每张 1 MiB。URL 去重，同一素材只下载一次；所有素材继续匿名请求并检查公网地址。

Dashboard「通用 → 图文卡片」对应 `render_cards = true`，关闭可恢复原图文消息。素材下载或绘图失败也会回退原图文，日志说明失败原因；后台音视频下载由原来的 `enable_media_download` 独立控制。`url状态` 显示当前回复模式。修改本次 Rust 源码后需重新构建并启动：

```bash
cargo run --release
```

各平台统一先发送解析卡片，再发送音视频。媒体下载与卡片渲染并行，下载先完成时等待该次卡片的成功发送回执；下载失败、体积/时长超限提示也在卡片之后发送。卡片渲染失败时，先发送原图文回退消息并等待其回执。卡片发送失败、回执等待超过 45 秒、请求取消或插件停用时，不发送后续媒体，也不自动重发结果未知的卡片。每次解析独立等待，不会被另一条链接的卡片提前放行。

卡片经 SDK 以图片字节发送，无需 QQ/OneBot 读取宿主的图片文件。素材请求带平台 Referer，不带平台登录 Cookie，使用配置的代理（留空直连）。正文素材抓取最多 20 秒，每张最大 12 MiB，合计最多 48 MiB；只允许公网 HTTP(S)，每次重定向重新验证地址。卡片宽 800 像素、高不超过 4096 像素，正文最多展示 10 张图；过长正文或更多图片会在底部提示查看原链接。字体随插件打包，见 `assets/fonts/README.md`；绘图本身不依赖浏览器或系统中文字体。抖音匿名网页解析的浏览器要求见下文。

素材准备与绘图随插件生命周期取消，整个流程同时处理最多 3 张卡片、等待最多 8 张，实际 CPU 绘图共用 SDK 配额。渲染代码全部位于本插件 `src/render/`，不增加独立 crate。

## Obscura page parsing and testing

This plugin enables `nuono-plugin`'s optional `obscura` Cargo feature. The framework SDK still leaves it disabled by default; compiling this plugin links the browser engine into the application. Run `cargo run` from the Nuono root to rebuild and start the application after this source change. Reloading an instance in Dashboard alone cannot incorporate the new dependency.

Obscura is enabled by default for Xiaohongshu, Kuaishou and QQ channel share pages. It executes page scripts and reads the resulting DOM and available page state. Douyin uses its API and plain HTTP share data first, then its separate anonymous Chromium fallback described below. Its full page is not executed inside Obscura because the current engine has a native iframe compatibility failure. Other platforms retain their existing API paths. Browser failures are reported without automatically retrying through plain HTTP, so a successful page result reflects the selected engine.

The Dashboard's **网页解析** section exposes:

| Configuration key | Default | Purpose |
| --- | --- | --- |
| `obscura_enabled` | `true` | Use Obscura for share pages; set `false` to compare with the previous HTTP parser. |
| `obscura_timeout_seconds` | `30` | Total browser timeout, including time waiting for a shared browser slot; range 1–120 seconds. |
| `obscura_settle_ms` | `1000` | Wait after loading for asynchronous scripts to populate data; range 0–10000 ms and less than the total timeout. |

After changing these values, reload the plugin instance or restart Nuono. Send `url状态` as an administrator to check the configured page engine, then send a supported share link to test its result. Page-fetch info logs identify `platform`, `engine` and whether page state was found (`has_state`), without logging cookies. For a comparison, disable `obscura_enabled`, reload the instance and resend the same link. Douyin links that succeed through its API do not exercise the browser.

HTTP share-page redirects are checked against the platform host allowlist at every hop. The browser path validates the initial and final platform hosts; intermediate browser navigations and subrequests follow the SDK's public-network rules. Browser output is capped at 8 MiB in total. The SDK shares one running browser task and eight queued tasks across plugins, so testing many links together can consume the timeout while queued.

To run the local real-engine fixture without starting Nuono or contacting platform sites, run from this plugin's directory:

```bash
cargo test --test obscura_smoke --locked
```

## 抖音匿名作品解析

抖音接口或 HTTP 分享页没有返回作品时，默认通过本机 Chrome、Edge 或 Chromium 的独立临时会话读取公开作品页。当前支持 macOS/Linux，不安装浏览器，不读取现有浏览器资料，也不向此会话注入 `douyin_cookie`。其他平台继续使用原有解析路径。没有浏览器或遇到站点验证限制时明确报错，不把通用错误页当作作品。

Dashboard「抖音」提供 `douyin_anonymous_browser`（默认 `true`）及 `douyin_browser_path`（留空自动查找，也可指定浏览器可执行文件）。`obscura_enabled` 不控制这一独立回退。临时会话继承插件显式代理；代理留空时直连，不读取环境代理。认证代理目前不支持，不会自动忽略代理配置。

提取匹配作品 ID 的静态封面、完整文案、作者昵称/头像及页面提供的发布时间。封面来自作品元数据，不截取播放帧；作者公开主页标识保留在解析数据，不冒充数值 UID。匿名页面播放器提供完整 MP4 地址时，也交给现有下载器；开启「下载 → 后台下载媒体」后会额外下载并发送视频，继续使用配置的体积上限、代理及并发限制。浏览器本身仍不播放或下载视频，不读取登录 Cookie。仅接受当前作品播放器内的抖音 CDN 地址，保留原始签名参数，排除推荐视频、音频、blob 和分片流；页面未提供可用地址时保留图文卡片，日志的 `has_video_stream` 标明是否拿到播放地址。此回退暂不解析图集。

开启图文卡片时，额外用最多 8 秒有限滚动读取匿名可见根评论，观察网页自身发起的对应作品评论/详情响应；不自行生成签名或注入登录信息。取得约 20 条候选后停止（上限 50 条），按评论 ID 去重、按点赞数降序展示最高 3 条。评论直接排在作者区下方，不显示排名标题；条目之间留白并以淡分隔线区分，点赞紧邻昵称。这不是全评论区排名，匿名会话返回的候选可能变化，也不展开楼中楼。头像和准确点赞数优先来自公开响应；同一评论的正文表情、配图/表情包从匹配的页面节点补齐，复用现有素材预算与静态渲染。

点赞、评论、收藏、分享统计优先使用准确接口计数，只有网页缩略值时保留「20.9万」等原显示值，不补造精确数字；作者累计获赞不会当作作品点赞。评论或附加素材缺失、超时、要求登录时保留基础海报；单张附图失败会显示未加载提示。关闭图文卡片时不进行评论采集。

浏览器会话最多运行 1 个、等待 2 个；排队和页面读取共用 40 秒时限。每次使用权限受限的新临时目录，调试连接只监听本机随机端口；成功、失败、取消及插件停用均关闭自有浏览器进程组、回收进程后删除目录。不会按进程名关闭用户的浏览器。HTML 输出限制为 4 MiB，只把公开作品字段交给渲染器，不保存页面会话或 Cookie。

这是本机浏览器进程，使用 Chromium 自带沙箱，不使用 `--no-sandbox`。顶层导航由经过校验的纯数字 ID 构造为固定抖音作品 URL；其脚本/子资源遵循 Chromium 网络行为，不具备 Obscura SDK 相同的公网子请求过滤。用于容器时需自行提供能以正常沙箱运行的浏览器及网络环境。

仅在明确需要真实联网验证时运行（不启动宿主，不读取本机插件配置或登录凭据）：

```bash
NUONO_DOUYIN_LIVE_OUTPUT=/tmp/nuono-douyin-preview \
cargo test --manifest-path plugins/LinkShowNN/Cargo.toml --lib --locked \
  anonymous_douyin_live_preview -- --ignored --nocapture
```

该测试使用 `https://v.douyin.com/B4nEt6lLK2w/`，验证作者/文案/封面/头像、统计及三条按点赞排序的评论，并输出实际海报和仅含公开字段的 `public-comments.json`。可用 `NUONO_DOUYIN_LIVE_PROXY` 显式设置测试代理；额外设置 `NUONO_DOUYIN_LIVE_DOWNLOAD=1` 时，通过生产下载器将 MP4 保存至输出目录的 `media/`，检查文件头，仍不向聊天发送消息。

## Source layout

```text
src/
  lib.rs                 # Public plugin type and factory
  plugin.rs              # Host lifecycle and resource assembly
  default_config.rs      # Defaults, parsing and Dashboard field declarations
  resolver/
    sites/               # Site adapters, each owning its required dependencies
      bilibili/          # Bilibili adapter and business logic
        mod.rs           # Adapter and internal module assembly
        content.rs       # Resolved cards and media
        client.rs        # Bilibili API requests
        error.rs         # API and parsing errors
        mapping.rs       # Upstream response conversion
        model.rs         # Response models
        streams.rs       # Stream models and quality selection
        urls.rs          # Bilibili link recognition
        tests.rs         # Bilibili parsing and mapping tests
    fetch.rs             # Share-page requests and redirects
    page.rs              # Shared HTML and JSON extraction
    url_policy.rs        # URL recognition and validation
    card.rs              # Display content
    content.rs           # Resolved-content contract
    media.rs             # Common share-page result
    site.rs              # Stable site names, domains, labels and Referer values
  delivery.rs            # SDK message segments and media thumbnail extension
  render/                # Asset fetching, bounded CPU drawing and PNG output
  download/              # Media model, bounded downloads and task delivery
  live/                  # Commands, subscriptions, file storage and polling
  runtime/               # Message pipeline, admission rules, metrics and state
```

Share-page adapters reuse the common fetch-and-enrich flow. Site-specific clients and settings belong to their adapters; the resolver assembles them once and routes requests. Bilibili's client, models and tests are ordinary modules inside the plugin, with no separate crate or library API. To add a site, declare its metadata in `resolver/site.rs`, implement its adapter under `resolver/sites/`, then register its constructor in `resolver/mod.rs`.

Card replies let the host fill the current conversation's native IDs and reply context. Background media and live notices use SDK message targets and routed delivery. Live subscription records retain their existing JSON fields and keys; optional channel metadata does not alter old group or private records.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). Bundled fonts retain their separate SIL Open Font License in `assets/fonts/OFL.txt`; icon source attribution is in `assets/icons/bilibili/README.md`.
