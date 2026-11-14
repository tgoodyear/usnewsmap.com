# Fonts served by check.usnewsmap.com

`NotoSansCherokee-Regular.woff2` is Noto Sans Cherokee, regular weight, so a Cherokee reading looks the same on every visitor's machine instead of depending on installed fonts. `style.css` loads it first in the stack, for the Cherokee blocks only (U+13A0-13FF and U+AB70-ABBF). The licence is the SIL Open Font License 1.1, in `OFL.txt` beside it. Copyright 2022 The Noto Project Authors.

| What         | Value                                                                                                                               |
| ------------ | ----------------------------------------------------------------------------------------------------------------------------------- |
| Source       | https://github.com/notofonts/cherokee, release `NotoSansCherokee-v2.001` (published 24 August 2022)                                 |
| Release file | `NotoSansCherokee-v2.001.zip`, sha256 `986b7fcf5c4cef9a7329aad8aea93d5c1b974e465eb7514f39517cace043a58d`                            |
| Font taken   | `NotoSansCherokee/full/ttf/NotoSansCherokee-Regular.ttf`, sha256 `cf06eed83a57d41cf873241370a7b6a389f93f4ddb42777b1be47628e45bce68` |
| Served file  | `NotoSansCherokee-Regular.woff2`, sha256 `031c99404395d012531834a74354d526e057e8147abe49fa98d52bc3d4b4edca`                         |
| Licence file | `OFL.txt` from the same release, sha256 `d7f9ca0cf1b9bcb7e94697bce31b2ec7a5571c51deb8f1f2f411ddf9a67859dd`                          |

The release has no woff2, so the served file is the TTF re-wrapped, with no glyph changed and no subsetting:

```sh
python3 -c "from fontTools.ttLib import TTFont; f = TTFont('NotoSansCherokee-Regular.ttf'); f.flavor = 'woff2'; f.save('NotoSansCherokee-Regular.woff2')"
```

(fontTools 4.68.0 with brotli 1.2.1 made the served file; a different version may give different bytes for the same font.) `fonts.test.mjs` checks the served file against the hash above. The look-alike check (`tools/lookalike.py`) draws Cherokee letters with this same file.
