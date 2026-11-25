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

## Letter key typefaces

A letter key (`edge/check/README.md`, "Letter key") shows printed pictures of each letter, cut from the task's own paper. A letter no page of the paper gives a clean picture of is drawn instead in a period typeface, named in the language's `glyph_font`: UnifrakturMaguntia for Fraktur German, IM Fell English for English. `style.css` declares both; the browser downloads one only when a key shows a letter in it. Both are served as released, unchanged TTF files (no re-wrapping or subsetting, so neither is a Modified Version under the licence), each with its own licence file. Both are under the SIL Open Font License 1.1.

| Font                                                                       | Source                                                                                                                                     | Served file, sha256                                                                               | Licence file, sha256                                                                             |
| -------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| UnifrakturMaguntia, Book. Copyright 2010 j. 'mach' wust, 2009 Peter Wiegel | https://github.com/google/fonts, `ofl/unifrakturmaguntia/UnifrakturMaguntia-Book.ttf` at commit `26be2797742d47542bd5e8e9153cea3a792fba85` | `UnifrakturMaguntia-Book.ttf`, `d64afc0547059dd2e4a78da48bda0aba0a9901be58c7f8c201a8b2b6b4492cc8` | `OFL-UnifrakturMaguntia.txt`, `96199bf9ff12dd471a66911dee95440737b437e7f5a6fd30e9e950c16d66e8df` |
| IM Fell English, Roman. Copyright 2010 Igino Marini                        | https://github.com/google/fonts, `ofl/imfellenglish/IMFeENrm28P.ttf` at commit `8d618a0e96499047423510abb2c5ee9f475b987a`                  | `IMFeENrm28P.ttf`, `fe9705bbde51af802719246d4608d08d37bde956ab99d9a590da996a5221a24c`             | `OFL-IMFellEnglish.txt`, `2a3ca501fc4d5efcad9798531e3e06962b1e20c60e464f6cbd6c17630112c773`      |

Cherokee keys use Noto Sans Cherokee above. Yiddish and Serbian have no bundled typeface, so a key for them leaves out a letter with no printed picture.
