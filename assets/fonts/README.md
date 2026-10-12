# Bundled Chinese font

`NotoSansSC-Regular.otf` is the unmodified Simplified Chinese subset of Noto Sans CJK, distributed under the SIL Open Font License 1.1 (see `OFL.txt`). Copyright and naming records are retained inside the font.

- Source: https://github.com/notofonts/noto-cjk/blob/main/Sans/SubsetOTF/SC/NotoSansSC-Regular.otf
- License: https://github.com/notofonts/noto-cjk/blob/main/Sans/LICENSE
- Retrieved: 2026-10-11
- SHA-256: `faa6c9df652116dde789d351359f3d7e5d2285a2b2a1f04a2d7244df706d5ea9`

The font is embedded in the plugin, so card rendering does not require system fonts, a browser screenshot service, or a font download at runtime.

The poster also embeds the unmodified `NotoSansSC-Bold.otf` from the same project and license:

- Source: https://github.com/notofonts/noto-cjk/blob/main/Sans/SubsetOTF/SC/NotoSansSC-Bold.otf
- Retrieved: 2026-10-11
- SHA-256: `c6cb5a93abaa9edc8ee7463b7ebb7f42d618d40e6ed2f7a5371c97b0b64767c0`

## Bundled color emoji

`NotoColorEmoji.ttf` is the unmodified CBDT/CBLC font from Google Noto Emoji, including flags. Copyright 2022 Google Inc. It is distributed under SIL Open Font License 1.1; see `NotoColorEmoji-OFL.txt`. The original copyright and naming records remain embedded in the font.

- Official repository commit: `e20cbc2bbec1926686be9f9bee7d1d2cfa1fea0e` (resolved using the GitHub API)
- Source: https://github.com/googlefonts/noto-emoji/blob/e20cbc2bbec1926686be9f9bee7d1d2cfa1fea0e/2D/fonts/NotoColorEmoji.ttf
- License: https://github.com/googlefonts/noto-emoji/blob/e20cbc2bbec1926686be9f9bee7d1d2cfa1fea0e/2D/fonts/LICENSE
- Retrieved: 2026-10-12
- Font SHA-256: `15671215ab769fdc7162a045d56fd7d7e477c51b04e6b3c761d914d8fdd6cc44`
- License SHA-256: `6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2`

LinkShowNN shapes complete emoji sequences with Rustybuzz and decodes the font's embedded PNG glyphs. This keeps emoji colors, skin tones, flags and joined sequences consistent across operating systems, without installed system fonts, login, or runtime font downloads. Text-style variation selectors continue to use the ordinary text font.
