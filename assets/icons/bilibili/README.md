# 海报统计图标

这四份 SVG 由用户于 2026-10-11 提供，来源为 B 站网页源码，保留原始路径和 `currentColor`：

- `like.svg`：点赞
- `coin.svg`：投币
- `star.svg`：收藏
- `share.svg`：分享

海报渲染器内嵌这些 SVG，用 resvg 将原始曲线转成抗锯齿透明蒙版，并缓存结果。按图形实际边界等比例缩放、居中，统一可见最大边 40 px、外框 42 px；颜色由 `src/render/poster.rs` 的 `ICON` 设置。替换 SVG 后重新构建插件即可生效，不需要手工导出 PNG。
