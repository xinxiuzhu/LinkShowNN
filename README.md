# LinkShowNN

[Nuono](https://github.com/xinxiuzhu/Nuono) 的多平台链接解析插件，发送链接即可查看图文卡片，并可下载音视频。

## 功能

- **多平台解析**：支持 B 站、抖音、快手、小红书、Twitter/X、网易云音乐、库街区和腾讯频道。
- **图文卡片**：将封面、文案、作者信息和可获取的统计数据渲染成一张图片。
- **热门评论**：B 站和抖音展示可获取评论中点赞最高的最多 3 条，支持头像、表情包和配图。
- **音视频下载**：开启后自动下载可获取的媒体，先发图文卡片，再发音视频。
- **直播订阅**：支持 B 站直播订阅与开播通知。

## 使用

在 Nuono 根目录执行：

```bash
git clone https://github.com/xinxiuzhu/LinkShowNN.git plugins/LinkShowNN
cargo run --release
```

在管理界面的插件配置中调整解析、卡片和下载选项，部分平台需额外配置。抖音匿名网页解析需本机安装 Chrome、Edge 或 Chromium（macOS / Linux）；实际可获取的内容以平台公开数据为准。

## 许可证

[Apache-2.0](LICENSE)。第三方素材说明见 [字体](assets/fonts/README.md) 和 [图标](assets/icons/bilibili/README.md)。
