# Third-party notices

Filo includes, or is built from, the work of other people. This file names each piece, its licence and its copyright
holders. Filo's own code is under the MIT licence, in [LICENSE](LICENSE).

The first sections are kept by hand: code copied or adapted into this repository, and the fonts, icons and pictures
the app bundles. The Rust crates at the end are generated from Cargo.lock by `scripts/third-party-notices.py`.

## Code copied or adapted into Filo

### gpui

Our copy of [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui) 0.2.2 is in `vendor/gpui`, used in
place of the crates.io release. Filo's changes are marked `Filo:` in the source and listed in `vendor/gpui/FILO.md`.

- Licence: Apache-2.0 (`vendor/gpui/LICENSE-APACHE`; the text is under "Licence texts" below)
- Copyright 2022 - 2025 Zed Industries, Inc.

### gpui-component

Our copy of [gpui-component](https://github.com/longbridge/gpui-component) 0.5.1 is in `vendor/gpui-component`, used in
place of the crates.io release. Filo's changes are marked `Filo:` in the source and listed in
`vendor/gpui-component/FILO.md`.

- Licence: Apache-2.0 (`vendor/gpui-component/LICENSE-APACHE`)
- Copyright 2024 - 2025 Longbridge

It carries, as upstream does, tree-sitter highlighting queries (`src/highlighter/languages`: MIT, from tree-sitter-rust,
-go, -javascript, -typescript and -zig, and Apache-2.0 for Markdown, from nvim-treesitter) and the shadcn/ui colour
palette (`src/theme/default-colors.json`, `default-theme.json`: MIT, Copyright (c) 2023 shadcn).

### kwwk-computer-use-core

The macOS Accessibility layer behind computer use, in `computer-use/vendor/kwwk-computer-use-core`, copied unchanged
from [EYHN/kwwk-computer-use-core](https://github.com/EYHN/kwwk-computer-use-core) at commit `eddd9e5`. It's built
into the app's computer use helper (`Contents/Resources/filo-computer-use`).

- Licence: MIT (`computer-use/vendor/kwwk-computer-use-core/LICENSE`)
- Copyright (c) 2026 EYHN

### Insomnia

Filo's "Keep running with the lid closed" (`src/local_backend/lid.rs`) is adapted from Insomnia's
`scripts/install.sh` and `scripts/backstop.sh`: the sudoers rule limited to the exact `pmset -a disablesleep`
commands and checked with `visudo -c` before it's installed, the journal of what was changed, and the launchd
backstop that runs only after a code-signature check and puts sleep back.

Source: https://github.com/krishhgg/Insomnia

```
MIT License

Copyright (c) 2026 Krish Garg

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Fonts

### Atkinson Hyperlegible Next

The app's typeface, `assets/fonts/AtkinsonHyperlegibleNext_*.ttf` (version 2.001), built into the app.

- Licence: SIL Open Font License 1.1, with no Reserved Font Name (`assets/fonts/OFL.txt`; the text is below)
- Copyright 2020-2024 The Atkinson Hyperlegible Next Project Authors (https://github.com/googlefonts/atkinson-hyperlegible-next)

### Google Sans

The website's typeface, `website/fonts/GoogleSans-*.woff2` (version 13.002). It's only on the website, not in the app.
"Google" and "Google Sans" are trademarks of Google LLC.

- Licence: SIL Open Font License 1.1, with no Reserved Font Name (`website/fonts/OFL.txt`)
- Copyright 2025 The Google Sans Project Authors (https://github.com/googlefonts/googlesans)

```
-----------------------------------------------------------
SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007
-----------------------------------------------------------

PREAMBLE
The goals of the Open Font License (OFL) are to stimulate worldwide
development of collaborative font projects, to support the font creation
efforts of academic and linguistic communities, and to provide a free and
open framework in which fonts may be shared and improved in partnership
with others.

The OFL allows the licensed fonts to be used, studied, modified and
redistributed freely as long as they are not sold by themselves. The
fonts, including any derivative works, can be bundled, embedded, 
redistributed and/or sold with any software provided that any reserved
names are not used by derivative works. The fonts and derivatives,
however, cannot be released under any other type of license. The
requirement for fonts to remain under this license does not apply
to any document created using the fonts or their derivatives.

DEFINITIONS
"Font Software" refers to the set of files released by the Copyright
Holder(s) under this license and clearly marked as such. This may
include source files, build scripts and documentation.

"Reserved Font Name" refers to any names specified as such after the
copyright statement(s).

"Original Version" refers to the collection of Font Software components as
distributed by the Copyright Holder(s).

"Modified Version" refers to any derivative made by adding to, deleting,
or substituting -- in part or in whole -- any of the components of the
Original Version, by changing formats or by porting the Font Software to a
new environment.

"Author" refers to any designer, engineer, programmer, technical
writer or other person who contributed to the Font Software.

PERMISSION & CONDITIONS
Permission is hereby granted, free of charge, to any person obtaining
a copy of the Font Software, to use, study, copy, merge, embed, modify,
redistribute, and sell modified and unmodified copies of the Font
Software, subject to the following conditions:

1) Neither the Font Software nor any of its individual components,
in Original or Modified Versions, may be sold by itself.

2) Original or Modified Versions of the Font Software may be bundled,
redistributed and/or sold with any software, provided that each copy
contains the above copyright notice and this license. These can be
included either as stand-alone text files, human-readable headers or
in the appropriate machine-readable metadata fields within text or
binary files as long as those fields can be easily viewed by the user.

3) No Modified Version of the Font Software may use the Reserved Font
Name(s) unless explicit written permission is granted by the corresponding
Copyright Holder. This restriction only applies to the primary font name as
presented to the users.

4) The name(s) of the Copyright Holder(s) or the Author(s) of the Font
Software shall not be used to promote, endorse or advertise any
Modified Version, except to acknowledge the contribution(s) of the
Copyright Holder(s) and the Author(s) or with their explicit written
permission.

5) The Font Software, modified or unmodified, in part or in whole,
must be distributed entirely under this license, and must not be
distributed under any other license. The requirement for fonts to
remain under this license does not apply to any document created
using the Font Software.

TERMINATION
This license becomes null and void if any of the above conditions are
not met.

DISCLAIMER
THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT
OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL THE
COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
INCLUDING ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL
DAMAGES, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM
OTHER DEALINGS IN THE FONT SOFTWARE.
```

## Icons and logos

### Lucide

The app's icons in `assets/icons` (all but the brand marks below, `filo-mark.svg`, `tab-artifacts.svg` and
`skills.svg`) are [Lucide](https://lucide.dev) icons; the `*-filled` ones are Lucide icons filled in. Lucide's icons
that come from [Feather](https://feathericons.com) are under Feather's MIT licence as well.

- Licence: ISC (the text is under "Licence texts" below); the parts from Feather: MIT
- Copyright (c) 2026 Lucide Icons and Contributors
- Copyright (c) 2013-present Cole Bemis (Feather)

### LobeHub Icons

`assets/icons/openai.svg` is from [LobeHub Icons](https://github.com/lobehub/lobe-icons).

- Licence: MIT
- Copyright (c) 2023 LobeHub

### Simple Icons

`assets/icons/claude.svg`, `meta.svg`, `opencode.svg` and `mcp-linear.svg`, `mcp-stripe.svg`, `mcp-huggingface.svg`,
`mcp-cloudflare-docs.svg`, `mcp-notion.svg`, `mcp-vercel.svg`, `mcp-github.svg`, `mcp-sentry.svg` and `mcp-clickup.svg`
are drawn from [Simple Icons](https://simpleicons.org) 16.34.0, which is released under CC0-1.0 (public domain), as are
the website's GitHub and X marks (`i-github` and `i-x-logo` in `website/*.html`).

### Services' own logos

`assets/icons/mcp-context7.svg` and `mcp-context7-dark.svg`, `mcp-playwright.svg`, `mcp-upsy.svg` and
`mcp-upsy-dark.svg`, `assets/images/mcp-deepwiki.png`, `mcp-executor.png` and `mcp-executor-dark.png` are the
services' logos as they publish them, used only to name each service in Extensions › Apps. `assets/icons/MCP-ICONS.md`
says where each came from.

### Trademarks

Claude and Anthropic, OpenAI and Codex, Meta, OpenCode, Linear, Stripe, Hugging Face, Cloudflare, Notion, Vercel,
GitHub, X, Sentry, ClickUp, Context7, Playwright, Upsy, DeepWiki, Executor and Google are trademarks of their owners.
Their logos appear in Filo only to name the service or tool they stand for. Filo isn't affiliated with or endorsed by
any of them.

## The note editor

`assets/web/note-editor.js`, the page behind the note and skill editors, is a minified bundle of
`web/note-editor.ts` with the packages below, built with esbuild (which left out their licence comments, so they're
here). It also has code from Filo's phone app (its Markdown ⇄ HTML note mapping), which is Filo's own. The versions are
those the bundle was built from, as far as they could be read back from it.

| Package | Version | Licence | Copyright |
| --- | --- | --- | --- |
| prosemirror-model, -state, -view, -transform, -history, -keymap, -commands, -schema-list, -dropcursor, -gapcursor | 1.25.12, 1.4.4, 1.42.6, 1.12.2, 1.5.1, 1.2.3, 1.7.2, 1.5.1, 1.8.4, 1.4.1 | MIT | Copyright (C) 2015-2017 by Marijn Haverbeke and others |
| orderedmap, w3c-keyname | 2.1.1, 2.2.8 | MIT | Copyright (C) 2016 by Marijn Haverbeke and others |
| rope-sequence | 1.3.4 | MIT | Copyright (C) 2016 by Marijn Haverbeke |
| @tiptap/core, @tiptap/pm, @tiptap/extensions and @tiptap/extension-document, -history, -paragraph, -text, -blockquote, -heading, -bold, -italic, -code, -strike, -list, -link | 3.27.1 | MIT | Copyright (c) 2025, Tiptap GmbH |
| react-native-enriched-html (its web formats, plugins and CSS) | 1.1.1 | MIT | Copyright (c) 2025 Software Mansion |
| markdown-it | 14.3.2 | MIT | Copyright (c) 2014 Vitaly Puzrin, Alex Kocharin |
| mdurl | 2.1.0 | MIT | Copyright (c) 2015 Vitaly Puzrin, Alex Kocharin |
| linkify-it | 5.0.2 | MIT | Copyright (c) 2015 Vitaly Puzrin |
| linkifyjs | 4.3.3 | MIT | Copyright (c) 2024 Nick Frasser |
| uc.micro, punycode.js | 2.1.0, 2.3.1 | MIT | Copyright Mathias Bynens |
| entities | 4.5.0 | BSD-2-Clause | Copyright (c) Felix Böhm |
| dompurify | 3.4.11 | MPL-2.0 OR Apache-2.0; used under Apache-2.0 | (c) Cure53 and other contributors |

The MIT, BSD-2-Clause and Apache-2.0 texts are under "Licence texts" below.

<!-- BEGIN generated by scripts/third-party-notices.py; don't edit by hand -->

## Rust crates

The 575 crates (from Cargo.lock) that the packaged app is built from. Where a crate is offered under a choice of licences, "Used under" is the one Filo takes. Generated by `scripts/third-party-notices.py`; run it again when Cargo.lock changes.

### Compiled into the app (481)

Their code is part of Filo's binary.

| Crate | Version | Licence | Used under | Copyright |
| --- | --- | --- | --- | --- |
| [adler2](https://github.com/oyvindln/adler2) | 2.0.1 | 0BSD OR MIT OR Apache-2.0 | MIT | Copyright (C) Jonas Schievink |
| [aho-corasick](https://github.com/BurntSushi/aho-corasick) | 1.1.5 | Unlicense OR MIT | MIT | Copyright (c) 2015 Andrew Gallant |
| [aligned](https://github.com/rust-embedded-community/aligned) | 0.4.3 | MIT OR Apache-2.0 | MIT | Copyright (c) 2017 Jorge Aparicio |
| [aligned-vec](https://github.com/sarah-ek/aligned-vec/) | 0.6.4 | MIT | MIT | Copyright (c) 2022 sarah |
| [anyhow](https://github.com/dtolnay/anyhow) | 1.0.104 | MIT OR Apache-2.0 | MIT | Copyright the anyhow authors (David Tolnay) |
| [arc-swap](https://github.com/vorner/arc-swap) | 1.9.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2017 arc-swap developers |
| [arrayref](https://github.com/droundy/arrayref) | 0.3.9 | BSD-2-Clause | BSD-2-Clause | Copyright (c) 2015 David Roundy |
| [arrayvec](https://github.com/bluss/arrayvec) | 0.7.8 | MIT OR Apache-2.0 | MIT | Copyright (c) Ulrik Sverdrup "bluss" 2015-2023 |
| [as-slice](https://github.com/japaric/as-slice) | 0.2.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 Jorge Aparicio |
| [async-channel](https://github.com/smol-rs/async-channel) | 1.9.0 | Apache-2.0 OR MIT | MIT | Copyright the async-channel authors (Stjepan Glavina) |
| [async-channel](https://github.com/smol-rs/async-channel) | 2.5.0 | Apache-2.0 OR MIT | MIT | Copyright the async-channel authors (Stjepan Glavina) |
| [async-compression](https://github.com/Nullus157/async-compression) | 0.4.50 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 the rustasync developers |
| [async-executor](https://github.com/smol-rs/async-executor) | 1.14.0 | Apache-2.0 OR MIT | MIT | Copyright the async-executor authors (Stjepan Glavina, John Nunley) |
| [async-fs](https://github.com/smol-rs/async-fs) | 2.2.0 | Apache-2.0 OR MIT | MIT | Copyright the async-fs authors (Stjepan Glavina) |
| [async-global-executor](https://github.com/Keruspe/async-global-executor) | 2.4.1 | Apache-2.0 OR MIT | MIT | Copyright the async-global-executor authors (Marc-Antoine Perennou) |
| [async-io](https://github.com/smol-rs/async-io) | 2.6.0 | Apache-2.0 OR MIT | MIT | Copyright the async-io authors (Stjepan Glavina) |
| [async-lock](https://github.com/smol-rs/async-lock) | 3.4.2 | Apache-2.0 OR MIT | MIT | Copyright the async-lock authors (Stjepan Glavina) |
| [async-net](https://github.com/smol-rs/async-net) | 2.0.0 | Apache-2.0 OR MIT | MIT | Copyright the async-net authors (Stjepan Glavina) |
| [async-process](https://github.com/smol-rs/async-process) | 2.5.0 | Apache-2.0 OR MIT | MIT | Copyright the async-process authors (Stjepan Glavina) |
| [async-signal](https://github.com/smol-rs/async-signal) | 0.2.14 | Apache-2.0 OR MIT | MIT | Copyright the async-signal authors (John Nunley) |
| [async-std](https://github.com/async-rs/async-std) | 1.13.2 | Apache-2.0 OR MIT | MIT | Copyright the async-std authors (Stjepan Glavina, Yoshua Wuyts, Friedel Ziegelmayer, Contributors to async-std) |
| [async-task](https://github.com/smol-rs/async-task) | 4.7.1 | Apache-2.0 OR MIT | MIT | Copyright the async-task authors (Stjepan Glavina) |
| [async_zip](https://github.com/Majored/rs-async-zip) | 0.0.17 | MIT | MIT | Copyright (c) 2021 Harry<br>Copyright (c) 2023 Cognite AS |
| [atomic](https://github.com/Amanieu/atomic-rs) | 0.5.3 | Apache-2.0/MIT | MIT | Copyright (c) 2016 The Rust Project Developers |
| [atomic-waker](https://github.com/smol-rs/atomic-waker) | 1.1.2 | Apache-2.0 OR MIT | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [av-scenechange](https://github.com/rust-av/av-scenechange) | 0.14.1 | MIT | MIT | Copyright (c) 2019 Multimedia and Rust |
| [av1-grain](https://github.com/rust-av/av1-grain) | 0.2.5 | BSD-2-Clause | BSD-2-Clause | Copyright (c) 2022-2022, the rav1e contributors |
| [avif-serialize](https://github.com/kornelski/avif-serialize) | 0.8.9 | BSD-3-Clause | BSD-3-Clause | Copyright (c) 2020, Cloudflare, Inc |
| [base62](https://github.com/fbernier/base62) | 2.2.6 | MIT | MIT | Copyright (c) 2015 François Bernier |
| [base64](https://github.com/marshallpierce/rust-base64) | 0.22.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 Alice Maz |
| [base64](https://github.com/marshallpierce/rust-base64) | 0.23.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2025 Alice Maz, Marshall Pierce |
| [bit_field](https://github.com/phil-opp/rust-bit-field) | 0.10.3 | Apache-2.0/MIT | MIT | Copyright (c) 2016 Philipp Oppermann |
| [bitflags](https://github.com/bitflags/bitflags) | 1.3.2 | MIT/Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [bitflags](https://github.com/bitflags/bitflags) | 2.13.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [bitstream-io](https://github.com/tuffy/bitstream-io) | 4.10.0 | MIT/Apache-2.0 | MIT | Copyright (c) 2017 Brian Langenberger |
| [block](http://github.com/SSheldon/rust-block) | 0.1.6 | MIT | MIT | Copyright the block authors (Steven Sheldon) |
| [block-buffer](https://github.com/RustCrypto/utils) | 0.10.4 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2019 The RustCrypto Project Developers |
| [block-buffer](https://github.com/RustCrypto/utils) | 0.12.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2025 The RustCrypto Project Developers |
| [block2](https://github.com/madsmtm/objc2) | 0.6.2 | MIT | MIT | Copyright the block2 authors (Mads Marquart) |
| [blocking](https://github.com/smol-rs/blocking) | 1.7.0 | Apache-2.0 OR MIT | MIT | Copyright the blocking authors |
| [bstr](https://github.com/BurntSushi/bstr) | 1.13.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2019 Andrew Gallant |
| [bytemuck](https://github.com/Lokathor/bytemuck) | 1.25.2 | Zlib OR Apache-2.0 OR MIT | MIT | Copyright (c) 2019 Daniel "Lokathor" Gee |
| [byteorder](https://github.com/BurntSushi/byteorder) | 1.5.0 | Unlicense OR MIT | MIT | Copyright (c) 2015 Andrew Gallant |
| [byteorder-lite](https://github.com/image-rs/byteorder-lite) | 0.1.0 | Unlicense OR MIT | MIT | Copyright (c) 2015 Andrew Gallant |
| [bytes](https://github.com/tokio-rs/bytes) | 1.12.1 | MIT | MIT | Copyright (c) 2018 Carl Lerche |
| [cfg-if](https://github.com/rust-lang/cfg-if) | 1.0.5 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [cgl](https://github.com/servo/cgl-rs) | 0.3.2 | MIT / Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [chacha20](https://github.com/RustCrypto/stream-ciphers) | 0.10.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2019-2026 The RustCrypto Project Developers |
| [chrono](https://github.com/chronotope/chrono) | 0.4.45 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014, Kang Seonghoon |
| [cocoa](https://github.com/servo/core-foundation-rs) | 0.26.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [cocoa-foundation](https://github.com/servo/core-foundation-rs) | 0.2.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [color_quant](https://github.com/image-rs/color_quant.git) | 1.1.0 | MIT | MIT | Copyright (c) 2016 PistonDevelopers |
| [command-fds](https://github.com/google/command-fds/) | 0.3.3 | Apache-2.0 | Apache-2.0 | Copyright the command-fds authors (Andrew Walbran) |
| [compression-codecs](https://github.com/Nullus157/async-compression) | 0.4.45 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 the rustasync developers |
| [compression-core](https://github.com/Nullus157/async-compression) | 0.4.33 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 the rustasync developers |
| [concurrent-queue](https://github.com/smol-rs/concurrent-queue) | 2.5.0 | Apache-2.0 OR MIT | MIT | Copyright the concurrent-queue authors (Stjepan Glavina, Taiki Endo, John Nunley) |
| [const-oid](https://github.com/RustCrypto/formats) | 0.10.2 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020-2026 The RustCrypto Project Developers |
| [cookie](https://github.com/SergioBenitez/cookie-rs) | 0.18.2 | MIT OR Apache-2.0 | MIT | Copyright 2017 Sergio Benitez<br>Copyright 2014 Alex Chricton<br>Copyright (c) 2017 Sergio Benitez<br>Copyright (c) 2014 Alex Crichton |
| [core-foundation](https://github.com/servo/core-foundation-rs) | 0.10.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [core-foundation](https://github.com/servo/core-foundation-rs) | 0.9.4 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [core-foundation-sys](https://github.com/servo/core-foundation-rs) | 0.8.7 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [core-graphics](https://github.com/servo/core-foundation-rs) | 0.24.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [core-graphics-types](https://github.com/servo/core-foundation-rs) | 0.1.3 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [core-graphics-types](https://github.com/servo/core-foundation-rs) | 0.2.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [core-graphics2](https://github.com/rust-media/apple-media-rs) | 0.4.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2024 Zhou Wei |
| [core-text](https://github.com/servo/core-foundation-rs) | 21.0.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [core-video](https://github.com/rust-media/apple-media-rs) | 0.4.3 | MIT OR Apache-2.0 | MIT | Copyright (c) 2024 Zhou Wei |
| [core_detect](https://github.com/thomcc/core_detect) | 1.0.0 | MIT/Apache-2.0 | MIT | Copyright (c) 2017-2020 The Rust Project Developers |
| [core_maths](https://github.com/robertbastian/core_maths) | 0.1.1 | MIT | MIT | Copyright (c) 2024 Robert Bastian |
| [cpufeatures](https://github.com/RustCrypto/utils) | 0.2.17 | MIT OR Apache-2.0 | MIT | Copyright (c) 2020-2025 The RustCrypto Project Developers |
| [cpufeatures](https://github.com/RustCrypto/utils) | 0.3.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2020-2026 The RustCrypto Project Developers |
| [crc32fast](https://github.com/srijs/rust-crc32fast) | 1.5.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 Sam Rijs, Alex Crichton and contributors |
| [crossbeam-deque](https://github.com/crossbeam-rs/crossbeam) | 0.8.8 | MIT OR Apache-2.0 | MIT | Copyright (c) 2019 The Crossbeam Project Developers |
| [crossbeam-epoch](https://github.com/crossbeam-rs/crossbeam) | 0.9.21 | MIT OR Apache-2.0 | MIT | Copyright (c) 2019 The Crossbeam Project Developers |
| [crossbeam-queue](https://github.com/crossbeam-rs/crossbeam) | 0.3.14 | MIT OR Apache-2.0 | MIT | Copyright (c) 2019 The Crossbeam Project Developers |
| [crossbeam-utils](https://github.com/crossbeam-rs/crossbeam) | 0.8.23 | MIT OR Apache-2.0 | MIT | Copyright (c) 2019 The Crossbeam Project Developers |
| [crypto-common](https://github.com/RustCrypto/traits) | 0.1.7 | MIT OR Apache-2.0 | MIT | Copyright (c) 2021 RustCrypto Developers |
| [crypto-common](https://github.com/RustCrypto/traits) | 0.2.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2021-2026 RustCrypto Developers |
| [ctor](https://github.com/mmastrac/rust-ctor) | 0.4.3 | Apache-2.0 OR MIT | MIT | Copyright the ctor authors (Matt Mastracci) |
| [data-encoding](https://github.com/ia0/data-encoding) | 2.11.1 | MIT | MIT | Copyright (c) 2015-2020 Julien Cretin<br>Copyright (c) 2017-2020 Google Inc |
| [data-url](https://github.com/servo/rust-url) | 0.3.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2013-2025 The rust-url developers |
| [deflate64](https://github.com/anatawa12/deflate64-rs) | 0.1.12 | MIT | MIT | Copyright (c) .NET Foundation and Contributors<br>Copyright (c) anatawa12 2023 |
| [deranged](https://github.com/jhpratt/deranged) | 0.5.8 | MIT OR Apache-2.0 | MIT | Copyright 2024 Jacob Pratt et al<br>Copyright (c) 2024 Jacob Pratt et al |
| [digest](https://github.com/RustCrypto/traits) | 0.10.7 | MIT OR Apache-2.0 | MIT | Copyright (c) 2017 Artyom Pavlov |
| [digest](https://github.com/RustCrypto/traits) | 0.11.3 | MIT OR Apache-2.0 | MIT | Copyright (c) 2017-2025 RustCrypto Developers<br>Copyright (c) 2017 Artyom Pavlov |
| [dirs](https://github.com/soc/dirs-rs) | 4.0.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2019 dirs-rs contributors |
| [dirs](https://github.com/soc/dirs-rs) | 5.0.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2019 dirs-rs contributors |
| [dirs](https://github.com/soc/dirs-rs) | 6.0.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2019 dirs-rs contributors |
| [dirs-sys](https://github.com/dirs-dev/dirs-sys-rs) | 0.3.7 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2019 dirs-rs contributors |
| [dirs-sys](https://github.com/dirs-dev/dirs-sys-rs) | 0.4.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2019 dirs-rs contributors |
| [dirs-sys](https://github.com/dirs-dev/dirs-sys-rs) | 0.5.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2019 dirs-rs contributors |
| [dispatch2](https://github.com/madsmtm/objc2) | 0.3.1 | Zlib OR Apache-2.0 OR MIT | MIT | Copyright the dispatch2 authors (Mads Marquart, Mary) |
| [dlib](https://github.com/elinorbgr/dlib) | 0.5.3 | MIT | MIT | Copyright (c) 2015 Victor Berger |
| [dpi](https://github.com/rust-windowing/winit) | 0.1.2 | Apache-2.0 AND MIT | MIT, Apache-2.0 | Copyright (c) 2018 Jorge Aparicio<br>copyright:<br>Copyright © 2005-2020 Rich Felker, et al<br>Copyright © 1993,2004 Sun Microsystems or<br>Copyright © 2003-2011 David Schultz or<br>Copyright © 2003-2009 Steven G. Kargl or<br>Copyright © 2003-2009 Bruce D. Evans or<br>Copyright © 2008 Stephen L. Moshier or<br>Copyright © 2017-2018 Arm Limited |
| [dtor](https://github.com/mmastrac/rust-ctor) | 0.0.6 | Apache-2.0 OR MIT | MIT | Copyright the dtor authors (Matt Mastracci) |
| [dunce](https://gitlab.com/kornelski/dunce) | 1.0.5 | CC0-1.0 OR MIT-0 OR Apache-2.0 | Apache-2.0 | Copyright the dunce authors (Kornel) |
| [dyn-clone](https://github.com/dtolnay/dyn-clone) | 1.0.20 | MIT OR Apache-2.0 | MIT | Copyright the dyn-clone authors (David Tolnay) |
| [either](https://github.com/rayon-rs/either) | 1.18.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 |
| [encoding_rs](https://github.com/hsivonen/encoding_rs) | 0.8.42 | (Apache-2.0 OR MIT) AND BSD-3-Clause | Apache-2.0, BSD-3-Clause | Copyright Mozilla Foundation<br>Copyright © WHATWG (Apple, Google, Mozilla, Microsoft) |
| [enum-iterator](https://github.com/stephaneyfx/enum-iterator.git) | 2.3.0 | 0BSD | 0BSD | Copyright (c) 2018 Stephane Raux |
| [equator](https://github.com/sarah-ek/equator/) | 0.4.2 | MIT | MIT | Copyright (c) 2023 sarah |
| [equivalent](https://github.com/indexmap-rs/equivalent) | 1.0.2 | Apache-2.0 OR MIT | MIT | Copyright (c) 2016--2023 |
| [erased-serde](https://github.com/dtolnay/erased-serde) | 0.4.10 | MIT OR Apache-2.0 | MIT | Copyright the erased-serde authors (David Tolnay) |
| [errno](https://github.com/lambda-fairy/rust-errno) | 0.3.14 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Chris Wong |
| [etagere](https://github.com/nical/etagere) | 0.2.15 | MIT/Apache-2.0 | MIT | Copyright 2020 Nicolas Silva<br>Copyright (c) 2020 Nicolas Silva |
| [euclid](https://github.com/servo/euclid) | 0.22.14 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [event-listener](https://github.com/smol-rs/event-listener) | 2.5.3 | Apache-2.0 OR MIT | MIT | Copyright the event-listener authors (Stjepan Glavina) |
| [event-listener](https://github.com/smol-rs/event-listener) | 5.4.2 | Apache-2.0 OR MIT | MIT | Copyright the event-listener authors (Stjepan Glavina, John Nunley) |
| [event-listener-strategy](https://github.com/smol-rs/event-listener-strategy) | 0.5.4 | Apache-2.0 OR MIT | MIT | Copyright the event-listener-strategy authors (John Nunley) |
| [exr](https://github.com/johannesvollmer/exrs) | 1.74.2 | BSD-3-Clause | BSD-3-Clause | Copyright (c) Contributors to the OpenEXR Project. All rights reserved<br>Copyright (c) Contributors to the exrs Project. All rights reserved |
| [fallible-iterator](https://github.com/sfackler/rust-fallible-iterator) | 0.3.0 | MIT/Apache-2.0 | MIT | Copyright (c) 2015 The rust-openssl-verify Developers |
| [fallible-streaming-iterator](https://github.com/sfackler/fallible-streaming-iterator) | 0.1.9 | MIT/Apache-2.0 | MIT | Copyright (c) 2016 The fallible-streaming-iterator Developers |
| [fastrand](https://github.com/smol-rs/fastrand) | 1.9.0 | Apache-2.0 OR MIT | MIT | Copyright the fastrand authors (Stjepan Glavina) |
| [fastrand](https://github.com/smol-rs/fastrand) | 2.5.0 | Apache-2.0 OR MIT | MIT | Copyright the fastrand authors (Stjepan Glavina) |
| [fax](https://github.com/pdf-rs/fax) | 0.2.7 | MIT | MIT | Copyright © 2021 The pdf-rs contributers |
| [fdeflate](https://github.com/image-rs/fdeflate) | 0.3.7 | MIT OR Apache-2.0 | MIT | Copyright the fdeflate authors (The image-rs Developers) |
| [filetime](https://github.com/alexcrichton/filetime) | 0.2.29 | MIT/Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [flate2](https://github.com/rust-lang/flate2-rs) | 1.1.10 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014-2026 Alex Crichton |
| [float-cmp](https://github.com/mikedilger/float-cmp) | 0.9.0 | MIT | MIT | Copyright (c) 2014-2020 Optimal Computing (NZ) Ltd |
| [float-ord](https://github.com/notriddle/rust-float-ord) | 0.3.2 | MIT / Apache-2.0 | MIT | Copyright (c) 2015 The rust-float-ord Developers |
| [float_next_after](https://gitlab.com/bronsonbdevost/next_afterf) | 1.0.0 | MIT | MIT | Copyright (c) 2020 Scripta Qumranica Electronica |
| [fluent-uri](https://github.com/yescallop/fluent-uri-rs) | 0.1.4 | MIT | MIT | Copyright (c) 2021 Scallop Ye |
| [fnv](https://github.com/servo/rust-fnv) | 1.0.7 | Apache-2.0 / MIT | MIT | Copyright (c) 2017 Contributors |
| [foldhash](https://github.com/orlp/foldhash) | 0.2.0 | Zlib | Zlib | Copyright (c) 2024 Orson Peters |
| [fontdb](https://github.com/RazrFalcon/fontdb) | 0.23.0 | MIT | MIT | Copyright (c) 2020 Yevhenii Reizner |
| [foreign-types](https://github.com/sfackler/foreign-types) | 0.5.0 | MIT/Apache-2.0 | MIT | Copyright (c) 2017 The foreign-types Developers |
| [foreign-types-shared](https://github.com/sfackler/foreign-types) | 0.3.1 | MIT/Apache-2.0 | MIT | Copyright (c) 2017 The foreign-types Developers |
| [form_urlencoded](https://github.com/servo/rust-url) | 1.2.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2013-2016 The rust-url developers |
| [fsevent-sys](https://github.com/octplane/fsevent-rust/tree/master/fsevent-sys) | 4.1.0 | MIT | MIT | Copyright (c) 2015 Pierre Baillet |
| [futf](https://github.com/servo/futf) | 0.1.5 | MIT / Apache-2.0 | MIT | Copyright (c) 2015 Keegan McAllister |
| [futures](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [futures-channel](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [futures-core](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [futures-executor](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [futures-io](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [futures-lite](https://github.com/smol-rs/futures-lite) | 1.13.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [futures-lite](https://github.com/smol-rs/futures-lite) | 2.6.1 | Apache-2.0 OR MIT | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [futures-sink](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [futures-task](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [futures-util](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [generic-array](https://github.com/fizyk20/generic-array.git) | 0.14.7 | MIT | MIT | Copyright (c) 2015 Bartłomiej Kamiński |
| [getrandom](https://github.com/rust-random/getrandom) | 0.2.17 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2024 The rust-random Project Developers<br>Copyright (c) 2014 The Rust Project Developers |
| [getrandom](https://github.com/rust-random/getrandom) | 0.3.4 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2025 The rust-random Project Developers<br>Copyright (c) 2014 The Rust Project Developers |
| [getrandom](https://github.com/rust-random/getrandom) | 0.4.3 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018-2026 The rust-random Project Developers<br>Copyright (c) 2014 The Rust Project Developers |
| [gif](https://github.com/image-rs/image-gif) | 0.13.3 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 nwin |
| [gif](https://github.com/image-rs/image-gif) | 0.14.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 nwin |
| [globset](https://github.com/BurntSushi/ripgrep/tree/master/crates/globset) | 0.4.20 | Unlicense OR MIT | MIT | Copyright (c) 2015 Andrew Gallant |
| [globwalk](https://github.com/gilnaa/globwalk) | 0.8.1 | MIT | MIT | Copyright (c) 2017 Gilad Naaman |
| [gpui](https://github.com/zed-industries/zed) (our copy: `vendor/gpui`) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [gpui-component](https://github.com/longbridge/gpui-component) (our copy: `vendor/gpui-component`) | 0.5.1 | Apache-2.0 | Apache-2.0 | Copyright 2024 - 2025 Longbridge <https://longbridge.com> |
| [gpui-component-assets](https://github.com/longbridge/gpui-component) | 0.5.1 | Apache-2.0 | Apache-2.0 | Copyright the gpui-component-assets authors |
| [gpui_collections](https://crates.io/crates/gpui_collections) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [gpui_http_client](https://crates.io/crates/gpui_http_client) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [gpui_media](https://crates.io/crates/gpui_media) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [gpui_refineable](https://crates.io/crates/gpui_refineable) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [gpui_semantic_version](https://crates.io/crates/gpui_semantic_version) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [gpui_sum_tree](https://crates.io/crates/gpui_sum_tree) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [gpui_util](https://crates.io/crates/gpui_util) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [grid](https://github.com/becheran/grid) | 0.18.0 | MIT | MIT | Copyright (c) 2020 Armin Becher |
| [h2](https://github.com/hyperium/h2) | 0.4.19 | MIT | MIT | Copyright (c) 2017 h2 authors |
| [half](https://github.com/VoidStarKat/half-rs) | 2.7.1 | MIT OR Apache-2.0 | MIT | Copyright the half authors (Kathryn Long) |
| [hashbrown](https://github.com/rust-lang/hashbrown) | 0.17.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Amanieu d'Antras |
| [hashlink](https://github.com/djc/hashlink) | 0.12.2 | MIT OR Apache-2.0 | MIT | Copyright the hashlink authors |
| [hex](https://github.com/KokaKiwi/rust-hex) | 0.4.3 | MIT OR Apache-2.0 | MIT | Copyright (c) 2013-2014 The Rust Project Developers<br>Copyright (c) 2015-2020 The rust-hex Developers |
| [home](https://github.com/rust-lang/cargo) | 0.5.12 | MIT OR Apache-2.0 | MIT | Copyright the home authors (Brian Anderson) |
| [html5ever](https://github.com/servo/html5ever) | 0.27.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The html5ever Project Developers |
| [http](https://github.com/hyperium/http) | 1.5.0 | MIT OR Apache-2.0 | MIT | Copyright 2017 http-rs authors<br>Copyright (c) 2017 http-rs authors |
| [http-body](https://github.com/hyperium/http-body) | 1.1.0 | MIT | MIT | Copyright (c) 2019-2026 Sean McArthur & Hyper Contributors |
| [http-body-util](https://github.com/hyperium/http-body) | 0.1.5 | MIT | MIT | Copyright (c) 2019-2026 Sean McArthur & Hyper Contributors |
| [httparse](https://github.com/seanmonstar/httparse) | 1.10.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015-2025 Sean McArthur |
| [hybrid-array](https://github.com/RustCrypto/hybrid-array) | 0.4.15 | MIT OR Apache-2.0 | MIT | Copyright (c) 2022-2026 The RustCrypto Project Developers |
| [hyper](https://github.com/hyperium/hyper) | 1.11.1 | MIT | MIT | Copyright (c) 2014-2026 Sean McArthur |
| [hyper-rustls](https://github.com/rustls/hyper-rustls) | 0.27.10 | Apache-2.0 OR ISC OR MIT | MIT | Copyright (c) 2016, Joseph Birr-Pixton<br>Copyright (c) 2016 Joseph Birr-Pixton |
| [hyper-util](https://github.com/hyperium/hyper-util) | 0.1.21 | MIT | MIT | Copyright (c) 2023-2025 Sean McArthur |
| [iana-time-zone](https://github.com/strawlab/iana-time-zone) | 0.1.65 | MIT OR Apache-2.0 | MIT | Copyright 2020 Andrew Straw<br>Copyright (c) 2020 Andrew D. Straw |
| [icu_collections](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [icu_locale_core](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [icu_normalizer](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [icu_normalizer_data](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [icu_properties](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [icu_properties_data](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [icu_provider](https://github.com/unicode-org/icu4x) | 2.3.1 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [idna](https://github.com/servo/rust-url/) | 1.1.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2013-2025 The rust-url developers |
| [idna_adapter](https://github.com/hsivonen/idna_adapter) | 1.2.2 | Apache-2.0 OR MIT | MIT | Copyright (c) The rust-url developers |
| [ignore](https://github.com/BurntSushi/ripgrep/tree/master/crates/ignore) | 0.4.33 | Unlicense OR MIT | MIT | Copyright (c) 2015 Andrew Gallant |
| [image](https://github.com/image-rs/image) | 0.25.10 | MIT OR Apache-2.0 | MIT | Copyright the image authors (The image-rs Developers) |
| [image-webp](https://github.com/image-rs/image-webp) | 0.2.4 | MIT OR Apache-2.0 | MIT | Copyright the image-webp authors |
| [imagesize](https://github.com/Roughsketch/imagesize) | 0.13.0 | MIT | MIT | Copyright (c) 2017 Maiddog |
| [imgref](https://github.com/kornelski/imgref) | 1.12.3 | CC0-1.0 OR Apache-2.0 | Apache-2.0 | Copyright the imgref authors (Kornel Lesiński) |
| [indexmap](https://github.com/indexmap-rs/indexmap) | 2.14.2 | Apache-2.0 OR MIT | MIT | Copyright (c) 2016--2017 |
| [instant](https://github.com/sebcrozet/instant) | 0.1.13 | BSD-3-Clause | BSD-3-Clause | Copyright (c) 2019, Sébastien Crozet |
| [inventory](https://github.com/dtolnay/inventory) | 0.3.24 | MIT OR Apache-2.0 | MIT | Copyright the inventory authors (David Tolnay) |
| [io-surface](https://github.com/servo/core-foundation-rs) | 0.16.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [ipnet](https://github.com/krisprice/ipnet) | 2.12.2 | MIT OR Apache-2.0 | MIT | Copyright 2017 Juniper Networks, Inc |
| [itertools](https://github.com/rust-itertools/itertools) | 0.11.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 |
| [itertools](https://github.com/rust-itertools/itertools) | 0.13.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 |
| [itertools](https://github.com/rust-itertools/itertools) | 0.14.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 |
| [itoa](https://github.com/dtolnay/itoa) | 1.0.18 | MIT OR Apache-2.0 | MIT | Copyright the itoa authors (David Tolnay) |
| [kurbo](https://github.com/linebender/kurbo) | 0.11.3 | Apache-2.0 OR MIT | MIT | Copyright (c) 2018 Raph Levien |
| [kv-log-macro](https://github.com/yoshuawuyts/kv-log-macro) | 1.0.7 | MIT OR Apache-2.0 | MIT | Copyright 2019 Yoshua Wuyts<br>Copyright (c) 2019 Yoshua Wuyts |
| [lazy_static](https://github.com/rust-lang-nursery/lazy-static.rs) | 1.5.1 | MIT OR Apache-2.0 | MIT | Copyright the lazy_static authors (Marvin Löbel) |
| [lb-wry](https://github.com/tauri-apps/wry) | 0.53.3 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020-2023 Ngo Iok Ui & Tauri Programme within The Commons Conservancy |
| [leak](https://github.com/jmesmon/leak.git) | 0.1.2 | Apache-2.0 OR MIT | MIT | Copyright the leak authors (Cody P Schafer) |
| [leaky-cow](https://github.com/notriddle/rust-leaky-cow) | 0.1.1 | MIT / Apache-2.0 | MIT | Copyright the leaky-cow authors (Michael Howell) |
| [lebe](https://github.com/johannesvollmer/lebe) | 0.5.3 | BSD-3-Clause | BSD-3-Clause | Copyright (c) 2022 Contributors to the lebe Project. All rights reserved |
| [libc](https://github.com/rust-lang/libc) | 0.2.189 | MIT OR Apache-2.0 | MIT | Copyright (c) The Rust Project Developers |
| [libloading](https://github.com/nagisa/rust_libloading/) | 0.8.9 | ISC | ISC | Copyright © 2015, Simonas Kazlauskas |
| [libm](https://github.com/rust-lang/compiler-builtins) | 0.2.16 | MIT | MIT | Copyright (c) 2018 Jorge Aparicio<br>copyright:<br>Copyright © 2005-2020 Rich Felker, et al<br>Copyright © 1993,2004 Sun Microsystems or<br>Copyright © 2003-2011 David Schultz or<br>Copyright © 2003-2009 Steven G. Kargl or<br>Copyright © 2003-2009 Bruce D. Evans or<br>Copyright © 2008 Stephen L. Moshier or<br>Copyright © 2017-2018 Arm Limited |
| [libsqlite3-sys](https://github.com/rusqlite/rusqlite) | 0.38.2 | MIT | MIT | Copyright (c) 2014 The rusqlite developers |
| [litemap](https://github.com/unicode-org/icu4x) | 0.8.3 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [lock_api](https://github.com/Amanieu/parking_lot) | 0.4.14 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 The Rust Project Developers |
| [log](https://github.com/rust-lang/log) | 0.4.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [loop9](https://gitlab.com/kornelski/loop9.git) | 0.1.5 | MIT | MIT | © Kornel Lesiński |
| [lru-slab](https://github.com/Ralith/lru-slab) | 0.1.3 | MIT OR Apache-2.0 OR Zlib | MIT | Copyright (c) 2024 The lru-slab Developers |
| [lsp-types](https://github.com/gluon-lang/lsp-types) | 0.97.0 | MIT | MIT | Copyright (c) 2016 Markus Westerlind |
| [lyon](https://github.com/nical/lyon) | 1.0.19 | MIT OR Apache-2.0 | MIT | Copyright the lyon authors (Nicolas Silva) |
| [lyon_algorithms](https://github.com/nical/lyon) | 1.0.21 | MIT OR Apache-2.0 | MIT | Copyright the lyon_algorithms authors (Nicolas Silva) |
| [lyon_geom](https://github.com/nical/lyon) | 1.0.19 | MIT OR Apache-2.0 | MIT | Copyright the lyon_geom authors (Nicolas Silva) |
| [lyon_path](https://github.com/nical/lyon) | 1.0.19 | MIT OR Apache-2.0 | MIT | Copyright the lyon_path authors (Nicolas Silva) |
| [lyon_tessellation](https://github.com/nical/lyon) | 1.0.22 | MIT OR Apache-2.0 | MIT | Copyright the lyon_tessellation authors (Nicolas Silva) |
| [mac](https://github.com/reem/rust-mac.git) | 0.1.1 | MIT/Apache-2.0 | MIT | Copyright the mac authors (Jonathan Reem) |
| [malloc_buf](https://github.com/SSheldon/malloc_buf) | 0.0.6 | MIT | MIT | Copyright the malloc_buf authors (Steven Sheldon) |
| [markdown](https://github.com/wooorm/markdown-rs) | 1.0.0 | MIT | MIT | Copyright (c) 2022 Titus Wormer |
| [markup5ever](https://github.com/servo/html5ever) | 0.12.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The html5ever Project Developers |
| [markup5ever_rcdom](https://github.com/servo/html5ever) | 0.3.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The html5ever Project Developers |
| [maybe-rayon](https://github.com/shssoichiro/maybe-rayon) | 0.1.1 | MIT | MIT | Copyright (c) 2021 Joshua Holmer |
| [memchr](https://github.com/BurntSushi/memchr) | 2.8.3 | Unlicense OR MIT | MIT | Copyright (c) 2015 Andrew Gallant |
| [memmap2](https://github.com/RazrFalcon/memmap2-rs) | 0.9.11 | MIT OR Apache-2.0 | MIT | Copyright [2015] [Dan Burkert]<br>Copyright (c) 2020 Yevhenii Reizner<br>Copyright (c) 2015 Dan Burkert |
| [metal](https://github.com/gfx-rs/metal-rs) | 0.29.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2010 The Rust Project Developers |
| [mime](https://github.com/hyperium/mime) | 0.3.17 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Sean McArthur |
| [mime_guess](https://github.com/abonander/mime_guess) | 2.0.5 | MIT | MIT | Copyright (c) 2015 Austin Bonander |
| [miniz_oxide](https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide) | 0.8.9 | MIT OR Zlib OR Apache-2.0 | MIT | Copyright 2013-2014 RAD Game Tools and Valve Software<br>Copyright 2010-2014 Rich Geldreich and Tenacious Software LLC<br>Copyright (c) 2017 Frommi<br>Copyright (c) 2017-2024 oyvindln<br>Copyright (c) 2020 Frommi |
| [miniz_oxide](https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide) | 0.9.1 | MIT OR Zlib OR Apache-2.0 | MIT | Copyright 2013-2014 RAD Game Tools and Valve Software<br>Copyright 2010-2014 Rich Geldreich and Tenacious Software LLC<br>Copyright (c) 2017 Frommi<br>Copyright (c) 2017-2024 oyvindln<br>Copyright (c) 2020 Frommi |
| [mio](https://github.com/tokio-rs/mio) | 1.2.3 | MIT | MIT | Copyright (c) 2014 Carl Lerche and other MIO contributors |
| [moxcms](https://github.com/awxkee/moxcms.git) | 0.8.1 | BSD-3-Clause OR Apache-2.0 | Apache-2.0 | Copyright 2024 Radzivon Bartoshyk<br>Copyright (c) Radzivon Bartoshyk. All rights reserved |
| [new_debug_unreachable](https://github.com/mbrubeck/rust-debug-unreachable) | 1.0.6 | MIT | MIT | Copyright (c) 2015 Jonathan Reem |
| [nix](https://github.com/nix-rust/nix) | 0.29.0 | MIT | MIT | Copyright (c) 2015 Carl Lerche + nix-rust Authors |
| [nix](https://github.com/nix-rust/nix) | 0.31.3 | MIT | MIT | Copyright (c) 2015 Carl Lerche + nix-rust Authors |
| [no_std_io2](https://github.com/wcampbell0x2a/no-std-io2) | 0.9.4 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020-2021  Brendan Molloy |
| [nom](https://github.com/rust-bakery/nom) | 8.0.0 | MIT | MIT | Copyright (c) 2014-2019 Geoffroy Couprie |
| [normpath](https://github.com/dylni/normpath) | 1.5.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2020 dylni (https://github.com/dylni)<br>Copyright (c) 2020 Nikolai Vazquez |
| [notify](https://github.com/notify-rs/notify.git) | 7.0.0 | CC0-1.0 | CC0-1.0 | Copyright the notify authors (Félix Saparelli, Daniel Faust, Aron Heinecke) |
| [notify-types](https://github.com/notify-rs/notify.git) | 1.0.1 | MIT OR Apache-2.0 | MIT | Copyright 2023 Notify Contributors<br>Copyright (c) 2023 Notify Contributors |
| [num-bigint](https://github.com/rust-num/num-bigint) | 0.4.8 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [num-complex](https://github.com/rust-num/num-complex) | 0.4.6 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [num-conv](https://github.com/jhpratt/num-conv) | 0.2.2 | MIT OR Apache-2.0 | MIT | Copyright (c) Jacob Pratt |
| [num-integer](https://github.com/rust-num/num-integer) | 0.1.47 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [num-rational](https://github.com/rust-num/num-rational) | 0.4.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [num-traits](https://github.com/rust-num/num-traits) | 0.2.19 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [num_cpus](https://github.com/seanmonstar/num_cpus) | 1.17.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015-2025 Sean McArthur |
| [objc](http://github.com/SSheldon/rust-objc) | 0.2.7 | MIT | MIT | Copyright (c) Steven Sheldon |
| [objc2](https://github.com/madsmtm/objc2) | 0.6.4 | MIT | MIT | Copyright the objc2 authors (Mads Marquart) |
| [objc2-app-kit](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | MIT | Copyright the objc2-app-kit authors |
| [objc2-av-foundation](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | MIT | Copyright the objc2-av-foundation authors |
| [objc2-avf-audio](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | MIT | Copyright the objc2-avf-audio authors |
| [objc2-core-foundation](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | MIT | Copyright the objc2-core-foundation authors |
| [objc2-encode](https://github.com/madsmtm/objc2) | 4.1.0 | MIT | MIT | Copyright the objc2-encode authors (Mads Marquart) |
| [objc2-exception-helper](https://github.com/madsmtm/objc2) | 0.1.1 | Zlib OR Apache-2.0 OR MIT | MIT | Copyright the objc2-exception-helper authors (Mads Marquart) |
| [objc2-foundation](https://github.com/madsmtm/objc2) | 0.3.2 | MIT | MIT | Copyright the objc2-foundation authors |
| [objc2-user-notifications](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | MIT | Copyright the objc2-user-notifications authors |
| [objc2-web-kit](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | MIT | Copyright the objc2-web-kit authors |
| [once_cell](https://github.com/matklad/once_cell) | 1.21.4 | MIT OR Apache-2.0 | MIT | Copyright the once_cell authors (Aleksey Kladov) |
| [open](https://github.com/Byron/open-rs) | 5.4.4 | MIT | MIT | Copyright © `2015` `Sebastian Thiel` |
| [option-ext](https://github.com/soc/option-ext.git) | 0.2.0 | MPL-2.0 | MPL-2.0 | Copyright the option-ext authors (Simon Ochsenreither) |
| [parking](https://github.com/smol-rs/parking) | 2.2.1 | Apache-2.0 OR MIT | MIT | Copyright 2014-2020 The Rust Project Developers |
| [parking_lot](https://github.com/Amanieu/parking_lot) | 0.12.5 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 The Rust Project Developers |
| [parking_lot_core](https://github.com/Amanieu/parking_lot) | 0.9.12 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 The Rust Project Developers |
| [pathfinder_geometry](https://github.com/servo/pathfinder) | 0.5.1 | MIT/Apache-2.0 | MIT | Copyright the pathfinder_geometry authors (Patrick Walton) |
| [pathfinder_simd](https://github.com/servo/pathfinder) | 0.5.6 | MIT OR Apache-2.0 | MIT | Copyright the pathfinder_simd authors (Patrick Walton) |
| [percent-encoding](https://github.com/servo/rust-url/) | 2.3.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2013-2025 The rust-url developers |
| [phf](https://github.com/rust-phf/rust-phf) | 0.11.3 | MIT | MIT | Copyright (c) 2014-2022 Steven Fackler, Yuki Okushi |
| [phf_shared](https://github.com/rust-phf/rust-phf) | 0.11.3 | MIT | MIT | Copyright (c) 2014-2022 Steven Fackler, Yuki Okushi |
| [pico-args](https://github.com/RazrFalcon/pico-args) | 0.5.0 | MIT | MIT | Copyright (c) 2019 Yevhenii Reizner |
| [pin-project](https://github.com/taiki-e/pin-project) | 1.1.13 | Apache-2.0 OR MIT | MIT | Copyright the pin-project authors |
| [pin-project-lite](https://github.com/taiki-e/pin-project-lite) | 0.2.17 | Apache-2.0 OR MIT | MIT | Copyright the pin-project-lite authors |
| [pin-utils](https://github.com/rust-lang/pin-utils) | 0.1.1 | MIT OR Apache-2.0 | MIT | Copyright 2018 The pin-utils authors<br>Copyright (c) 2018 The pin-utils authors |
| [piper](https://github.com/smol-rs/piper) | 0.2.5 | MIT OR Apache-2.0 | MIT | Copyright the piper authors (Stjepan Glavina, John Nunley) |
| [png](https://github.com/image-rs/image-png) | 0.17.16 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 nwin |
| [png](https://github.com/image-rs/image-png) | 0.18.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 nwin |
| [polling](https://github.com/smol-rs/polling) | 3.11.0 | Apache-2.0 OR MIT | MIT | Copyright the polling authors (Stjepan Glavina, John Nunley) |
| [pollster](https://github.com/zesterer/pollster) | 0.2.5 | Apache-2.0/MIT | MIT | Copyright (c) 2020-2021 Joshua Barretto |
| [postage](https://github.com/austinjones/postage-rs) | 0.5.0 | MIT | MIT | Copyright (c) 2022 Austin Jones |
| [potential_utf](https://github.com/unicode-org/icu4x) | 0.1.6 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [powerfmt](https://github.com/jhpratt/powerfmt) | 0.2.0 | MIT OR Apache-2.0 | MIT | Copyright 2023 Jacob Pratt et al<br>Copyright (c) 2023 Jacob Pratt et al |
| [ppv-lite86](https://github.com/cryptocorrosion/cryptocorrosion) | 0.2.21 | MIT OR Apache-2.0 | MIT | Copyright 2019 The CryptoCorrosion Contributors<br>Copyright (c) 2019 The CryptoCorrosion Contributors |
| [precomputed-hash](https://github.com/emilio/precomputed-hash) | 0.1.1 | MIT | MIT | Copyright (c) 2017 Emilio Cobos Álvarez |
| [proc-macro2](https://github.com/dtolnay/proc-macro2) | 1.0.107 | MIT OR Apache-2.0 | MIT | Copyright the proc-macro2 authors (David Tolnay, Alex Crichton) |
| [profiling](https://github.com/aclysma/profiling) | 1.0.18 | MIT OR Apache-2.0 | MIT | Copyright the profiling authors (Philip Degarmo) |
| [psm](https://github.com/rust-lang/stacker/) | 0.1.32 | MIT OR Apache-2.0 | MIT | Copyright © 2018, Simonas Kazlauskas |
| [pulp](https://github.com/sarah-quinones/pulp/) | 0.22.3 | MIT | MIT | Copyright (c) 2021 sarah |
| [pulp-wasm-simd-flag](https://github.com/sarah-quinones/pulp/) | 0.1.1 | MIT | MIT | Copyright the pulp-wasm-simd-flag authors (sarah quiñones) |
| [pxfm](https://github.com/awxkee/pxfm) | 0.1.30 | BSD-3-Clause OR Apache-2.0 | Apache-2.0 | Copyright 2024 Radzivon Bartoshyk<br>Copyright (c) Radzivon Bartoshyk. All rights reserved |
| [qoi](https://github.com/aldanor/qoi-rust) | 0.4.1 | MIT/Apache-2.0 | MIT | Copyright (c) 2022 Ivan Smirnov |
| [quick-error](http://github.com/tailhook/quick-error) | 2.0.1 | MIT/Apache-2.0 | MIT | Copyright (c) 2015 The quick-error Developers |
| [quinn](https://github.com/quinn-rs/quinn) | 0.11.12 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 The quinn Developers |
| [quinn-proto](https://github.com/quinn-rs/quinn) | 0.11.19 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 The quinn Developers |
| [quinn-udp](https://github.com/quinn-rs/quinn) | 0.5.16 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 The quinn Developers |
| [rand](https://github.com/rust-random/rand) | 0.10.3 | MIT OR Apache-2.0 | MIT | copyright assignment is required to contribute to the Rand project<br>Copyright 2018 Developers of the Rand project<br>Copyright (c) 2014 The Rust Project Developers |
| [rand](https://github.com/rust-random/rand) | 0.9.5 | MIT OR Apache-2.0 | MIT | copyright assignment is required to contribute to the Rand project<br>Copyright 2018 Developers of the Rand project<br>Copyright (c) 2014 The Rust Project Developers |
| [rand_chacha](https://github.com/rust-random/rand) | 0.9.0 | MIT OR Apache-2.0 | MIT | copyright assignment is required to contribute to the Rand project<br>Copyright 2018 Developers of the Rand project<br>Copyright (c) 2014 The Rust Project Developers |
| [rand_core](https://github.com/rust-random/rand_core) | 0.10.1 | MIT OR Apache-2.0 | MIT | copyright assignment is required to contribute to the Rand project<br>Copyright (c) 2018-2026 The Rand Project Developers |
| [rand_core](https://github.com/rust-random/rand) | 0.6.4 | MIT OR Apache-2.0 | MIT | copyright assignment is required to contribute to the Rand project<br>Copyright 2018 Developers of the Rand project<br>Copyright (c) 2014 The Rust Project Developers |
| [rand_core](https://github.com/rust-random/rand) | 0.9.5 | MIT OR Apache-2.0 | MIT | copyright assignment is required to contribute to the Rand project<br>Copyright 2018 Developers of the Rand project<br>Copyright (c) 2014 The Rust Project Developers |
| [rand_pcg](https://github.com/rust-random/rngs) | 0.10.2 | MIT OR Apache-2.0 | MIT | copyright assignment is required to contribute to the Rand project<br>Copyright (c) 2014-2017 Melissa O'Neill and PCG Project contributors<br>Copyright 2018 Developers of the Rand project |
| [rav1e](https://github.com/xiph/rav1e/) | 0.8.1 | BSD-2-Clause | BSD-2-Clause | Copyright (c) 2017-2023, the rav1e contributors |
| [ravif](https://github.com/kornelski/cavif-rs) | 0.13.0 | BSD-3-Clause | BSD-3-Clause | Copyright (c) 2020, Kornel |
| [raw-cpuid](https://github.com/gz/rust-cpuid) | 11.6.0 | MIT | MIT | Copyright (c) 2015 Gerd Zellweger |
| [raw-window-handle](https://github.com/rust-windowing/raw-window-handle) | 0.6.2 | MIT OR Apache-2.0 OR Zlib | MIT | Copyright (c) 2019 Osspial<br>Copyright (c) 2020 Osspial |
| [rayon](https://github.com/rayon-rs/rayon) | 1.12.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2010 The Rust Project Developers |
| [rayon-core](https://github.com/rayon-rs/rayon) | 1.13.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2010 The Rust Project Developers |
| [reborrow](https://github.com/sarah-ek/reborrow/) | 0.5.5 | MIT | MIT | Copyright (c) 2022 sarah |
| [ref-cast](https://github.com/dtolnay/ref-cast) | 1.0.27 | MIT OR Apache-2.0 | MIT | Copyright the ref-cast authors (David Tolnay) |
| [regex](https://github.com/rust-lang/regex) | 1.13.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [regex-automata](https://github.com/rust-lang/regex) | 0.4.18 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [regex-syntax](https://github.com/rust-lang/regex) | 0.8.11 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [reqwest](https://github.com/seanmonstar/reqwest) | 0.12.28 | MIT OR Apache-2.0 | MIT | Copyright 2016 Sean McArthur<br>Copyright (c) 2016-2025 Sean McArthur |
| [resvg](https://github.com/linebender/resvg) | 0.45.1 | Apache-2.0 OR MIT | MIT | Copyright 2017 the Resvg Authors |
| [rgb](https://github.com/kornelski/rust-rgb) | 0.8.53 | MIT | MIT | Copyright (c) 2019 Kornel |
| [ring](https://github.com/briansmith/ring) | 0.17.14 | Apache-2.0 AND ISC | Apache-2.0, ISC | Copyright (c) 2009 The Go Authors. All rights reserved<br>Copyright 2015 The Chromium Authors. All rights reserved<br>Copyright 2015-2025 Brian Smith |
| [ropey](https://github.com/cessen/ropey) | 2.0.0-beta.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2023 Nathan Vegdahl |
| [roxmltree](https://github.com/RazrFalcon/roxmltree) | 0.20.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 Yevhenii Reizner |
| [rusqlite](https://github.com/rusqlite/rusqlite) | 0.40.2 | MIT | MIT | Copyright (c) 2014 The rusqlite developers |
| [rust-embed](https://pyrossh.dev/repos/rust-embed) | 8.12.0 | MIT | MIT | Copyright (c) 2018 pyros2097 |
| [rust-embed-utils](https://pyrossh.dev/repos/rust-embed) | 8.12.0 | MIT | MIT | Copyright (c) 2018 pyros2097 |
| [rust-i18n](https://github.com/longbridge/rust-i18n) | 3.1.5 | MIT | MIT | Copyright (c) 2021 Longbridge |
| [rust-i18n-support](https://github.com/longbridge/rust-i18n) | 3.1.5 | MIT | MIT | Copyright the rust-i18n-support authors |
| [rustc-hash](https://github.com/rust-lang/rustc-hash) | 2.1.3 | Apache-2.0 OR MIT | MIT | Copyright the rustc-hash authors (The Rust Project Developers) |
| [rustix](https://github.com/bytecodealliance/rustix) | 0.38.44 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | MIT | Copyright the rustix authors (Dan Gohman, Jakub Konka) |
| [rustix](https://github.com/bytecodealliance/rustix) | 1.1.5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | MIT | Copyright the rustix authors (Dan Gohman, Jakub Konka) |
| [rustls](https://github.com/rustls/rustls) | 0.23.45 | Apache-2.0 OR ISC OR MIT | MIT | Copyright (c) 2016, Joseph Birr-Pixton<br>Copyright (c) 2016 Joseph Birr-Pixton |
| [rustls-native-certs](https://github.com/rustls/rustls-native-certs) | 0.8.4 | Apache-2.0 OR ISC OR MIT | MIT | Copyright (c) 2016, Joseph Birr-Pixton<br>Copyright (c) 2016 Joseph Birr-Pixton |
| [rustls-pemfile](https://github.com/rustls/pemfile) | 2.2.0 | Apache-2.0 OR ISC OR MIT | MIT | Copyright (c) 2016, Joseph Birr-Pixton<br>Copyright (c) 2016 Joseph Birr-Pixton |
| [rustls-pki-types](https://github.com/rustls/pki-types) | 1.15.1 | MIT OR Apache-2.0 | MIT | Copyright 2023 Dirkjan Ochtman<br>Copyright (c) 2023 Dirkjan Ochtman |
| [rustls-webpki](https://github.com/rustls/webpki) | 0.103.15 | ISC | ISC | Copyright 2015 Brian Smith |
| [rustybuzz](https://github.com/harfbuzz/rustybuzz) | 0.20.1 | MIT | MIT | Copyright (c) HarfBuzz developers<br>Copyright (c) 2020 Yevhenii Reizner |
| [ryu](https://github.com/dtolnay/ryu) | 1.0.23 | Apache-2.0 OR BSL-1.0 | Apache-2.0 | Copyright the ryu authors (David Tolnay) |
| [same-file](https://github.com/BurntSushi/same-file) | 1.0.6 | Unlicense/MIT | MIT | Copyright (c) 2017 Andrew Gallant |
| [schemars](https://github.com/GREsau/schemars) | 1.2.2 | MIT | MIT | Copyright (c) 2019 Graham Esau |
| [scopeguard](https://github.com/bluss/scopeguard) | 1.2.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016-2019 Ulrik Sverdrup "bluss" and scopeguard developers |
| [seahash](https://gitlab.redox-os.org/redox-os/seahash) | 4.1.0 | MIT | MIT | Copyright the seahash authors (ticki, Tom Almeida) |
| [security-framework](https://github.com/kornelski/rust-security-framework) | 3.7.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 Steven Fackler |
| [security-framework-sys](https://github.com/kornelski/rust-security-framework) | 2.17.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 Steven Fackler |
| [serde](https://github.com/serde-rs/serde) | 1.0.229 | MIT OR Apache-2.0 | MIT | Copyright the serde authors (Erick Tryzelaar, David Tolnay) |
| [serde_core](https://github.com/serde-rs/serde) | 1.0.229 | MIT OR Apache-2.0 | MIT | Copyright the serde_core authors (Erick Tryzelaar, David Tolnay) |
| [serde_fmt](https://github.com/KodrAus/serde_fmt.git) | 1.1.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2019 Ashley Mannix |
| [serde_json](https://github.com/serde-rs/json) | 1.0.151 | MIT OR Apache-2.0 | MIT | Copyright the serde_json authors (Erick Tryzelaar, David Tolnay) |
| [serde_json_lenient](https://github.com/google/serde_json_lenient) | 0.2.4 | MIT/Apache-2.0 | MIT | Copyright the serde_json_lenient authors (Adrian Taylor, Michael Bolin, Erick Tryzelaar, David Tolnay) |
| [serde_spanned](https://github.com/toml-rs/toml) | 0.6.9 | MIT OR Apache-2.0 | MIT | Copyright (c) Individual contributors |
| [serde_urlencoded](https://github.com/nox/serde_urlencoded) | 0.7.1 | MIT/Apache-2.0 | MIT | Copyright (c) 2016 Anthony Ramine |
| [serde_yaml](https://github.com/dtolnay/serde-yaml) | 0.9.34+deprecated | MIT OR Apache-2.0 | MIT | Copyright the serde_yaml authors (David Tolnay) |
| [sha1](https://github.com/RustCrypto/hashes) | 0.10.7 | MIT OR Apache-2.0 | MIT | Copyright (c) 2006-2009 Graydon Hoare<br>Copyright (c) 2009-2013 Mozilla Foundation<br>Copyright (c) 2016 Artyom Pavlov |
| [sha1_smol](https://github.com/mitsuhiko/sha1-smol) | 1.0.1 | BSD-3-Clause | BSD-3-Clause | Copyright (c) 2018, the respective contributors, as shown by the AUTHORS file<br>Copyright (c) 2006-2009 Graydon Hoare<br>Copyright (c) 2009-2013 Mozilla Foundation |
| [sha2](https://github.com/RustCrypto/hashes) | 0.10.9 | MIT OR Apache-2.0 | MIT | Copyright (c) 2006-2009 Graydon Hoare<br>Copyright (c) 2009-2013 Mozilla Foundation<br>Copyright (c) 2016 Artyom Pavlov |
| [sha2](https://github.com/RustCrypto/hashes) | 0.11.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016-2026 The RustCrypto Project Developers<br>Copyright (c) 2016 Artyom Pavlov<br>Copyright (c) 2009-2013 Mozilla Foundation<br>Copyright (c) 2006-2009 Graydon Hoare |
| [shlex](https://github.com/comex/rust-shlex) | 1.3.0 | MIT OR Apache-2.0 | MIT | Copyright 2015 Nicholas Allegra (comex)<br>Copyright (c) 2015 Nicholas Allegra (comex) |
| [signal-hook-registry](https://github.com/vorner/signal-hook) | 1.4.8 | MIT OR Apache-2.0 | MIT | Copyright (c) 2017 tokio-jsonrpc developers |
| [simd-adler32](https://github.com/mcountryman/simd-adler32) | 0.3.10 | MIT | MIT | Copyright (c) [2021] [Marvin Countryman] |
| [simdutf8](https://github.com/rusticstuff/simdutf8) | 0.1.5 | MIT OR Apache-2.0 | MIT | Copyright the simdutf8 authors (Hans Kratz) |
| [simplecss](https://github.com/linebender/simplecss) | 0.2.2 | Apache-2.0 OR MIT | MIT | Copyright (c) 2018 Reizner Evgeniy |
| [siphasher](https://github.com/jedisct1/rust-siphash) | 1.0.4 | MIT OR Apache-2.0 | MIT | Copyright 2012-2016 The Rust Project Developers<br>Copyright 2016-2026 Frank Denis |
| [slab](https://github.com/tokio-rs/slab) | 0.4.12 | MIT | MIT | Copyright (c) 2019 Carl Lerche |
| [slotmap](https://github.com/orlp/slotmap) | 1.1.1 | Zlib | Zlib | Copyright (c) 2021 Orson Peters |
| [smallvec](https://github.com/servo/rust-smallvec) | 1.16.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 The Servo Project Developers |
| [smol](https://github.com/smol-rs/smol) | 2.0.2 | Apache-2.0 OR MIT | MIT | Copyright the smol authors (Stjepan Glavina) |
| [socket2](https://github.com/rust-lang/socket2) | 0.6.5 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [spin](https://github.com/mvdnes/spin-rs.git) | 0.9.9 | MIT | MIT | Copyright (c) 2014 Mathijs van de Nes |
| [stable_deref_trait](https://github.com/storyyeller/stable_deref_trait) | 1.2.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2017 Robert Grosse |
| [stacker](https://github.com/rust-lang/stacker) | 0.1.25 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [stacksafe](https://github.com/fast/stacksafe) | 0.1.4 | Apache-2.0 | Apache-2.0 | Copyright the stacksafe authors |
| [static_assertions](https://github.com/nvzqz/static-assertions-rs) | 1.1.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2017 Nikolai Vazquez |
| [str_indices](https://github.com/cessen/str_indices) | 0.4.4 | MIT OR Apache-2.0 | MIT | Copyright the str_indices authors (Nathan Vegdahl) |
| [streaming-iterator](https://github.com/sfackler/streaming-iterator) | 0.1.9 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Steven Fackler |
| [strict-num](https://github.com/RazrFalcon/strict-num) | 0.1.1 | MIT | MIT | Copyright (c) 2022 Yevhenii Reizner |
| [string_cache](https://github.com/servo/string-cache) | 0.8.9 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [strum](https://github.com/Peternator7/strum) | 0.27.2 | MIT | MIT | Copyright (c) 2019 Peter Glotfelty |
| [subtle](https://github.com/dalek-cryptography/subtle) | 2.6.1 | BSD-3-Clause | BSD-3-Clause | Copyright (c) 2016-2017 Isis Agora Lovecruft, Henry de Valence. All rights reserved<br>Copyright (c) 2016-2024 Isis Agora Lovecruft. All rights reserved |
| [sval](https://github.com/sval-rs/sval) | 2.22.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020 sval-rs |
| [sval_buffer](https://github.com/sval-rs/sval) | 2.22.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020 sval-rs |
| [sval_dynamic](https://github.com/sval-rs/sval) | 2.22.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020 sval-rs |
| [sval_fmt](https://github.com/sval-rs/sval) | 2.22.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020 sval-rs |
| [sval_json](https://github.com/sval-rs/sval) | 2.22.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020 sval-rs |
| [sval_nested](https://github.com/sval-rs/sval) | 2.22.0 | Apache-2.0 OR MIT | MIT | Copyright the sval_nested authors (Ashley Mannix) |
| [sval_ref](https://github.com/sval-rs/sval) | 2.22.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020 sval-rs |
| [sval_serde](https://github.com/sval-rs/sval) | 2.22.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020 sval-rs |
| [svg_fmt](https://github.com/nical/rust_debug) | 0.4.5 | MIT/Apache-2.0 | MIT | Copyright the svg_fmt authors (Nicolas Silva) |
| [svgtypes](https://github.com/linebender/svgtypes) | 0.15.3 | Apache-2.0 OR MIT | MIT | Copyright (c) 2018 Yevhenii Reizner |
| [sync_wrapper](https://github.com/Actyx/sync_wrapper) | 1.0.2 | Apache-2.0 | Apache-2.0 | Copyright the sync_wrapper authors (Actyx AG) |
| [system-configuration](https://github.com/mullvad/system-configuration-rs) | 0.6.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2024 Mullvad VPN AB |
| [system-configuration-sys](https://github.com/mullvad/system-configuration-rs) | 0.6.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2024 Mullvad VPN AB |
| [taffy](https://github.com/DioxusLabs/taffy) | 0.9.0 | MIT | MIT | Copyright the taffy authors (Alice Cecile, Johnathan Kelley, Nico Burns) |
| [take-until](https://github.com/hdevalke/take-until.git) | 0.2.0 | MIT | MIT | Copyright 2019 Hannes De Valkeneer |
| [tempfile](https://github.com/Stebalien/tempfile) | 3.27.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 Steven Allen |
| [tendril](https://github.com/servo/tendril) | 0.4.3 | MIT/Apache-2.0 | MIT | Copyright (c) 2015 Keegan McAllister |
| [thiserror](https://github.com/dtolnay/thiserror) | 1.0.69 | MIT OR Apache-2.0 | MIT | Copyright the thiserror authors (David Tolnay) |
| [thiserror](https://github.com/dtolnay/thiserror) | 2.0.21 | MIT OR Apache-2.0 | MIT | Copyright the thiserror authors (David Tolnay) |
| [tiff](https://github.com/image-rs/image-tiff) | 0.11.3 | MIT | MIT | Copyright (c) 2018 PistonDevelopers |
| [time](https://github.com/time-rs/time) | 0.3.55 | MIT OR Apache-2.0 | MIT | Copyright (c) Jacob Pratt et al |
| [time-core](https://github.com/time-rs/time) | 0.1.9 | MIT OR Apache-2.0 | MIT | Copyright (c) Jacob Pratt et al |
| [tiny-skia](https://github.com/RazrFalcon/tiny-skia) | 0.11.4 | BSD-3-Clause | BSD-3-Clause | Copyright (c) 2011 Google Inc. All rights reserved<br>Copyright (c) 2020 Yevhenii Reizner All rights reserved |
| [tiny-skia-path](https://github.com/RazrFalcon/tiny-skia/tree/master/path) | 0.11.4 | BSD-3-Clause | BSD-3-Clause | Copyright (c) 2011 Google Inc. All rights reserved<br>Copyright (c) 2020 Yevhenii Reizner All rights reserved |
| [tinystr](https://github.com/unicode-org/icu4x) | 0.8.4 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [tinyvec](https://github.com/Lokathor/tinyvec) | 1.13.3 | Zlib OR Apache-2.0 OR MIT | MIT | Copyright (c) 2019 Daniel "Lokathor" Gee |
| [tokio](https://github.com/tokio-rs/tokio) | 1.53.1 | MIT | MIT | Copyright (c) Tokio Contributors |
| [tokio-rustls](https://github.com/rustls/tokio-rustls) | 0.26.6 | MIT OR Apache-2.0 | MIT | Copyright 2017 quininer kel<br>Copyright (c) 2017 quininer kel |
| [tokio-socks](https://github.com/sticnarf/tokio-socks) | 0.5.3 | MIT | MIT | Copyright (c) 2018 Yilin Chen |
| [tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) | 0.28.0 | MIT | MIT | Copyright (c) 2017 Daniel Abramov<br>Copyright (c) 2017 Alexey Galakhov |
| [tokio-util](https://github.com/tokio-rs/tokio) | 0.7.19 | MIT | MIT | Copyright (c) Tokio Contributors |
| [toml](https://github.com/toml-rs/toml) | 0.8.23 | MIT OR Apache-2.0 | MIT | Copyright (c) Individual contributors |
| [toml_datetime](https://github.com/toml-rs/toml) | 0.6.11 | MIT OR Apache-2.0 | MIT | Copyright (c) Individual contributors |
| [toml_edit](https://github.com/toml-rs/toml) | 0.22.27 | MIT OR Apache-2.0 | MIT | Copyright (c) Individual contributors |
| [toml_write](https://github.com/toml-rs/toml) | 0.1.2 | MIT OR Apache-2.0 | MIT | Copyright (c) Individual contributors |
| [tower](https://github.com/tower-rs/tower) | 0.5.3 | MIT | MIT | Copyright (c) 2019 Tower Contributors |
| [tower-http](https://github.com/tower-rs/tower-http) | 0.6.11 | MIT | MIT | Copyright (c) 2019-2021 Tower Contributors |
| [tower-layer](https://github.com/tower-rs/tower) | 0.3.3 | MIT | MIT | Copyright (c) 2019 Tower Contributors |
| [tower-service](https://github.com/tower-rs/tower) | 0.3.3 | MIT | MIT | Copyright (c) 2019 Tower Contributors |
| [tracing](https://github.com/tokio-rs/tracing) | 0.1.44 | MIT | MIT | Copyright (c) 2019 Tokio Contributors |
| [tracing-core](https://github.com/tokio-rs/tracing) | 0.1.36 | MIT | MIT | Copyright (c) 2019 Tokio Contributors |
| [tree-sitter](https://github.com/tree-sitter/tree-sitter) | 0.25.10 | MIT | MIT | Copyright the tree-sitter authors (Max Brunsfeld, Amaan Qureshi) |
| [tree-sitter-json](https://github.com/tree-sitter/tree-sitter-json) | 0.24.8 | MIT | MIT | Copyright the tree-sitter-json authors (Max Brunsfeld, Amaan Qureshi) |
| [tree-sitter-language](https://github.com/tree-sitter/tree-sitter) | 0.1.8 | MIT | MIT | Copyright (c) 2018 Max Brunsfeld |
| [triomphe](https://github.com/Manishearth/triomphe) | 0.1.16 | MIT OR Apache-2.0 | MIT | Copyright (c) 2019 Manish Goregaokar |
| [try-lock](https://github.com/seanmonstar/try-lock) | 0.2.5 | MIT | MIT | Copyright (c) 2018-2023 Sean McArthur<br>Copyright (c) 2016 Alex Crichton |
| [ttf-parser](https://github.com/harfbuzz/ttf-parser) | 0.25.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2018 Yevhenii Reizner |
| [tungstenite](https://github.com/snapview/tungstenite-rs) | 0.28.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2017 Alexey Galakhov<br>Copyright (c) 2016 Jason Housley |
| [typeid](https://github.com/dtolnay/typeid) | 1.0.3 | MIT OR Apache-2.0 | MIT | Copyright the typeid authors (David Tolnay) |
| [typenum](https://github.com/paholg/typenum) | 1.20.1 | MIT OR Apache-2.0 | MIT | Copyright 2014 Paho Lurie-Gregg<br>Copyright (c) 2014 Paho Lurie-Gregg |
| [unicase](https://github.com/seanmonstar/unicase) | 2.9.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014-2026 Sean McArthur |
| [unicode-bidi](https://github.com/servo/unicode-bidi) | 0.3.18 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 The Rust Project Developers |
| [unicode-bidi-mirroring](https://github.com/RazrFalcon/unicode-bidi-mirroring) | 0.4.0 | MIT/Apache-2.0 | MIT | Copyright (c) 2020 Yevhenii Reizner |
| [unicode-ccc](https://github.com/RazrFalcon/unicode-ccc) | 0.4.0 | MIT/Apache-2.0 | MIT | Copyright (c) 2020 Yevhenii Reizner |
| [unicode-id](https://github.com/Boshen/unicode-id) | 0.3.7 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 The Rust Project Developers |
| [unicode-ident](https://github.com/dtolnay/unicode-ident) | 1.0.26 | (MIT OR Apache-2.0) AND Unicode-3.0 | MIT, Unicode-3.0 | Copyright © 1991-2023 Unicode, Inc |
| [unicode-properties](https://github.com/unicode-rs/unicode-properties) | 0.1.4 | MIT/Apache-2.0 | MIT | Copyright (c) 2015 The Rust Project Developers |
| [unicode-script](https://github.com/unicode-rs/unicode-script) | 0.5.8 | MIT OR Apache-2.0 | MIT | Copyright 2021 The Unicode-rs Developers<br>Copyright (c) 2019 Manish Goregaokar |
| [unicode-segmentation](https://github.com/unicode-rs/unicode-segmentation) | 1.13.3 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 The Rust Project Developers |
| [unicode-vo](https://github.com/RazrFalcon/unicode-vo) | 0.1.0 | MIT/Apache-2.0 | MIT | Copyright (c) 2018 Reizner Evgeniy |
| [unsafe-libyaml](https://github.com/dtolnay/unsafe-libyaml) | 0.2.11 | MIT | MIT | Copyright the unsafe-libyaml authors (David Tolnay) |
| [untrusted](https://github.com/briansmith/untrusted) | 0.9.0 | ISC | ISC | Copyright 2015-2016 Brian Smith |
| [url](https://github.com/servo/rust-url) | 2.5.8 | MIT OR Apache-2.0 | MIT | Copyright (c) 2013-2025 The rust-url developers |
| [usvg](https://github.com/linebender/resvg) | 0.45.1 | Apache-2.0 OR MIT | MIT | Copyright 2017 the Resvg Authors |
| [utf-8](https://github.com/SimonSapin/rust-utf8) | 0.7.6 | MIT OR Apache-2.0 | MIT | Copyright the utf-8 authors (Simon Sapin) |
| [utf8_iter](https://github.com/hsivonen/utf8_iter) | 1.0.4 | Apache-2.0 OR MIT | MIT | Copyright Mozilla Foundation |
| [uuid](https://github.com/uuid-rs/uuid) | 1.26.1 | Apache-2.0 OR MIT | MIT | Copyright (c) 2014 The Rust Project Developers<br>Copyright (c) 2018 Ashley Mannix, Christopher Armstrong, Dylan DPC, Hunar Roop Kahlon |
| [v_frame](https://github.com/rust-av/v_frame) | 0.3.9 | BSD-2-Clause | BSD-2-Clause | Copyright (c) 2017-2022, the rav1e contributors |
| [value-bag](https://github.com/sval-rs/value-bag) | 1.14.1 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020 sval-rs |
| [value-bag-serde1](https://crates.io/crates/value-bag-serde1) | 1.14.1 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020 sval-rs |
| [value-bag-sval2](https://crates.io/crates/value-bag-sval2) | 1.14.1 | Apache-2.0 OR MIT | MIT | Copyright (c) 2020 sval-rs |
| [waker-fn](https://github.com/smol-rs/waker-fn) | 1.2.0 | Apache-2.0 OR MIT | MIT | Copyright the waker-fn authors (Stjepan Glavina) |
| [walkdir](https://github.com/BurntSushi/walkdir) | 2.5.0 | Unlicense/MIT | MIT | Copyright (c) 2015 Andrew Gallant |
| [want](https://github.com/seanmonstar/want) | 0.3.1 | MIT | MIT | Copyright (c) 2018-2019 Sean McArthur |
| [wasm-bindgen](https://github.com/wasm-bindgen/wasm-bindgen) | 0.2.129 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [wasm-bindgen-shared](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared) | 0.2.129 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [webpki-roots](https://github.com/rustls/webpki-roots) | 0.26.11 | CDLA-Permissive-2.0 | CDLA-Permissive-2.0 | Copyright the webpki-roots authors |
| [webpki-roots](https://github.com/rustls/webpki-roots) | 1.0.9 | CDLA-Permissive-2.0 | CDLA-Permissive-2.0 | Copyright the webpki-roots authors |
| [weezl](https://github.com/image-rs/weezl) | 0.1.12 | MIT OR Apache-2.0 | MIT | Copyright (c) HeroicKatora 2020 |
| [which](https://github.com/harryfei/which-rs.git) | 6.0.3 | MIT | MIT | Copyright (c) 2015 fangyuanziti |
| [winnow](https://github.com/winnow-rs/winnow) | 0.7.15 | MIT | MIT | Copyright the winnow authors |
| [workspace-hack](https://github.com/facebookincubator/cargo-guppy) | 0.1.0 | CC0-1.0 | CC0-1.0 | Copyright the workspace-hack authors |
| [writeable](https://github.com/unicode-org/icu4x) | 0.6.4 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [xattr](https://github.com/Stebalien/xattr) | 0.2.3 | MIT/Apache-2.0 | MIT | Copyright (c) 2015 Steven Allen |
| [xml5ever](https://github.com/servo/html5ever) | 0.18.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The html5ever Project Developers |
| [xmlwriter](https://github.com/RazrFalcon/xmlwriter) | 0.1.0 | MIT | MIT | Copyright (c) 2019 Reizner Evgeniy |
| [y4m](https://github.com/image-rs/y4m.git) | 0.8.0 | MIT | MIT | Copyright (c) 2015-2019 PistonDevelopers<br>Copyright (c) 2019 image-rs contributors |
| [yeslogic-fontconfig-sys](https://github.com/yeslogic/fontconfig-rs) | 6.0.1 | MIT | MIT | Copyright 2019 YesLogic Pty. Ltd<br>Copyright 2016 Manuel Reinhardt<br>Copyright 2014 Austin Bonander<br>Copyright 2013 The Servo Project Developers |
| [yoke](https://github.com/unicode-org/icu4x) | 0.8.3 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [zed-async-tar](https://github.com/dignifiedquire/async-tar) | 0.5.0-zed | MIT/Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [zed-font-kit](https://github.com/servo/font-kit) | 0.14.1-zed | MIT OR Apache-2.0 | MIT | Copyright the zed-font-kit authors (Patrick Walton) |
| [zed-reqwest](https://github.com/seanmonstar/reqwest) | 0.12.15-zed | MIT OR Apache-2.0 | MIT | Copyright 2016 Sean McArthur<br>Copyright (c) 2016-2025 Sean McArthur |
| [zed-sum-tree](https://crates.io/crates/zed-sum-tree) | 0.2.0 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [zerocopy](https://github.com/google/zerocopy) | 0.8.59 | BSD-2-Clause OR Apache-2.0 OR MIT | MIT | Copyright 2023 The Fuchsia Authors<br>Copyright 2019 The Fuchsia Authors |
| [zerofrom](https://github.com/unicode-org/icu4x) | 0.1.8 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [zeroize](https://github.com/RustCrypto/utils) | 1.9.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2018-2026 The RustCrypto Project Developers |
| [zerotrie](https://github.com/unicode-org/icu4x) | 0.2.5 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [zerovec](https://github.com/unicode-org/icu4x) | 0.11.8 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [zlib-rs](https://github.com/trifectatechfoundation/zlib-rs) | 0.6.8 | Zlib | Zlib | (C) 2024 Trifecta Tech Foundation |
| [zmij](https://github.com/dtolnay/zmij) | 1.0.23 | MIT | MIT | Copyright the zmij authors (David Tolnay) |
| [zune-core](https://crates.io/crates/zune-core) | 0.4.12 | MIT OR Apache-2.0 OR Zlib | MIT | Copyright the zune-core authors |
| [zune-core](https://github.com/etemesi254/zune-image) | 0.5.3 | MIT OR Apache-2.0 OR Zlib | MIT | Copyright (c) zune-image developers |
| [zune-inflate](https://crates.io/crates/zune-inflate) | 0.2.54 | MIT OR Apache-2.0 OR Zlib | MIT | Copyright the zune-inflate authors |
| [zune-jpeg](https://github.com/etemesi254/zune-image/tree/dev/crates/zune-jpeg) | 0.4.21 | MIT OR Apache-2.0 OR Zlib | MIT | Copyright the zune-jpeg authors (caleb) |
| [zune-jpeg](https://github.com/etemesi254/zune-image/tree/dev/crates/zune-jpeg) | 0.5.15 | MIT OR Apache-2.0 OR Zlib | MIT | Copyright (c) zune-image developers |

### Used only to build it (94)

Build scripts, code generators and macros: they run while Filo is built, and none of their code is in the app.

| Crate | Version | Licence | Used under | Copyright |
| --- | --- | --- | --- | --- |
| [ar_archive_writer](https://github.com/rust-lang/ar_archive_writer) | 0.5.3 | Apache-2.0 WITH LLVM-exception | Apache-2.0 WITH LLVM-exception | Copyright the ar_archive_writer authors |
| [arg_enum_proc_macro](https://github.com/lu-zero/arg_enum_proc_macro) | 0.3.4 | MIT | MIT | Copyright (c) 2018 Luca Barbato |
| [autocfg](https://github.com/cuviper/autocfg) | 1.5.1 | Apache-2.0 OR MIT | MIT | Copyright (c) 2018 Josh Stone |
| [bindgen](https://github.com/rust-lang/rust-bindgen) | 0.71.1 | BSD-3-Clause | BSD-3-Clause | Copyright (c) 2013, Jyun-Yan You |
| [bit-set](https://github.com/contain-rs/bit-set) | 0.8.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2023 The Rust Project Developers |
| [bit-vec](https://github.com/contain-rs/bit-vec) | 0.8.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2023 The Rust Project Developers |
| [built](https://github.com/lukaslueg/built) | 0.8.1 | MIT | MIT | Copyright (c) 2017-2023 Lukas Lueg |
| [bumpalo](https://github.com/fitzgen/bumpalo) | 3.20.3 | MIT OR Apache-2.0 | MIT | Copyright (c) 2019 Nick Fitzgerald |
| [cbindgen](https://github.com/mozilla/cbindgen) | 0.28.0 | MPL-2.0 | MPL-2.0 | Copyright the cbindgen authors (Emilio Cobos Álvarez, Jeff Muizelaar, Kartikaya Gupta, Ryan Hunt) |
| [cc](https://github.com/rust-lang/cc-rs) | 1.5.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [cexpr](https://github.com/jethrogb/rust-cexpr) | 0.6.0 | Apache-2.0/MIT | MIT | Copyright the cexpr authors (Jethro Beekman) |
| [cfg_aliases](https://github.com/katharostech/cfg_aliases) | 0.2.2 | MIT | MIT | Copyright (c) 2020 Katharos Technology |
| [clang-sys](https://github.com/KyleMayes/clang-sys) | 1.9.1 | Apache-2.0 | Apache-2.0 | Copyright the clang-sys authors (Kyle Mayes) |
| [codespan-reporting](https://github.com/brendanzab/codespan) | 0.12.0 | Apache-2.0 | Apache-2.0 | Copyright the codespan-reporting authors (Brendan Zabarauskas) |
| [convert_case](https://github.com/rutrum/convert-case) | 0.4.0 | MIT | MIT | Copyright the convert_case authors (David Purdum) |
| [ctor-proc-macro](https://github.com/mmastrac/rust-ctor) | 0.0.6 | Apache-2.0 OR MIT | MIT | Copyright the ctor-proc-macro authors (Matt Mastracci) |
| [derive_more](https://github.com/JelteF/derive_more) | 0.99.20 | MIT | MIT | Copyright (c) 2016 Jelte Fennema |
| [displaydoc](https://github.com/yaahc/displaydoc) | 0.2.7 | MIT OR Apache-2.0 | MIT | Copyright the displaydoc authors (Jane Lusby) |
| [dtor-proc-macro](https://github.com/mmastrac/rust-ctor) | 0.0.5 | Apache-2.0 OR MIT | MIT | Copyright the dtor-proc-macro authors (Matt Mastracci) |
| [enum-iterator-derive](https://github.com/stephaneyfx/enum-iterator.git) | 1.5.0 | 0BSD | 0BSD | Copyright (c) 2018 Stephane Raux |
| [equator-macro](https://github.com/sarah-ek/equator/) | 0.4.2 | MIT | MIT | Copyright (c) 2023 sarah |
| [find-msvc-tools](https://github.com/rust-lang/cc-rs) | 0.1.14 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [foldhash](https://github.com/orlp/foldhash) | 0.1.5 | Zlib | Zlib | Copyright (c) 2024 Orson Peters |
| [foreign-types-macros](https://github.com/sfackler/foreign-types) | 0.2.4 | MIT/Apache-2.0 | MIT | Copyright (c) 2017 The foreign-types Developers |
| [futures-macro](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Alex Crichton<br>Copyright (c) 2017 The Tokio Authors |
| [glob](https://github.com/rust-lang/glob) | 0.3.4 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [gpui-component-macros](https://crates.io/crates/gpui-component-macros) | 0.5.1 | Apache-2.0 | Apache-2.0 | Copyright 2024 - 2025 Longbridge <https://longbridge.com> |
| [gpui-macros](https://crates.io/crates/gpui-macros) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [gpui_derive_refineable](https://crates.io/crates/gpui_derive_refineable) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [gpui_perf](https://crates.io/crates/gpui_perf) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [gpui_util_macros](https://crates.io/crates/gpui_util_macros) | 0.2.2 | Apache-2.0 | Apache-2.0 | Copyright 2022 - 2025 Zed Industries, Inc |
| [hashbrown](https://github.com/rust-lang/hashbrown) | 0.15.5 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 Amanieu d'Antras |
| [heck](https://github.com/withoutboats/heck) | 0.4.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 The Rust Project Developers |
| [heck](https://github.com/withoutboats/heck) | 0.5.0 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 The Rust Project Developers |
| [hexf-parse](https://github.com/lifthrasiir/hexf) | 0.2.1 | CC0-1.0 | CC0-1.0 | Copyright the hexf-parse authors (Kang Seonghoon) |
| [jobserver](https://github.com/rust-lang/jobserver-rs) | 0.1.35 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [minimal-lexical](https://github.com/Alexhuszagh/minimal-lexical) | 0.2.1 | MIT/Apache-2.0 | MIT | Copyright (c) 2009 The Go Authors. All rights reserved |
| [multiversion_no_op](https://github.com/hsivonen/multiversion_no_op) | 1.0.0 | Apache-2.0 OR MIT | MIT | Copyright Mozilla Foundation |
| [naga](https://github.com/gfx-rs/wgpu/tree/trunk/naga) | 25.0.1 | MIT OR Apache-2.0 | MIT | Copyright the naga authors (gfx-rs developers) |
| [nom](https://github.com/Geal/nom) | 7.1.3 | MIT | MIT | Copyright (c) 2014-2019 Geoffroy Couprie |
| [noop_proc_macro](https://github.com/lu-zero/noop_proc_macro) | 0.3.0 | MIT | MIT | Copyright (c) 2019 Luca Barbato |
| [num-derive](https://github.com/rust-num/num-derive) | 0.4.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 The Rust Project Developers |
| [object](https://github.com/gimli-rs/object) | 0.39.1 | Apache-2.0 OR MIT | MIT | Copyright (c) 2015 The Gimli Developers |
| [paste](https://github.com/dtolnay/paste) | 1.0.15 | MIT OR Apache-2.0 | MIT | Copyright the paste authors (David Tolnay) |
| [pastey](https://github.com/as1100k/pastey) | 0.1.1 | MIT OR Apache-2.0 | MIT | Copyright the pastey authors (Aditya Kumar, David Tolnay) |
| [phf_codegen](https://github.com/rust-phf/rust-phf) | 0.11.3 | MIT | MIT | Copyright (c) 2014-2022 Steven Fackler, Yuki Okushi |
| [phf_generator](https://github.com/rust-phf/rust-phf) | 0.11.3 | MIT | MIT | Copyright (c) 2014-2022 Steven Fackler, Yuki Okushi |
| [pin-project-internal](https://github.com/taiki-e/pin-project) | 1.1.13 | Apache-2.0 OR MIT | MIT | Copyright the pin-project-internal authors |
| [pkg-config](https://github.com/rust-lang/pkg-config-rs) | 0.3.34 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [prettyplease](https://github.com/dtolnay/prettyplease) | 0.2.37 | MIT OR Apache-2.0 | MIT | Copyright the prettyplease authors (David Tolnay) |
| [proc-macro-error-attr2](https://github.com/GnomedDev/proc-macro-error-2) | 2.0.0 | MIT OR Apache-2.0 | MIT | Copyright 2019-2020 CreepySkeleton<br>Copyright (c) 2019-2020 CreepySkeleton |
| [proc-macro-error2](https://github.com/GnomedDev/proc-macro-error-2) | 2.0.1 | MIT OR Apache-2.0 | MIT | Copyright 2019-2020 CreepySkeleton<br>Copyright (c) 2019-2020 CreepySkeleton |
| [profiling-procmacros](https://github.com/aclysma/profiling) | 1.0.18 | MIT OR Apache-2.0 | MIT | Copyright the profiling-procmacros authors (Philip Degarmo) |
| [quote](https://github.com/dtolnay/quote) | 1.0.47 | MIT OR Apache-2.0 | MIT | Copyright the quote authors (David Tolnay) |
| [rand](https://github.com/rust-random/rand) | 0.8.8 | MIT OR Apache-2.0 | MIT | copyright assignment is required to contribute to the Rand project<br>Copyright 2018 Developers of the Rand project<br>Copyright (c) 2014 The Rust Project Developers |
| [rand_chacha](https://github.com/rust-random/rand) | 0.3.1 | MIT OR Apache-2.0 | MIT | copyright assignment is required to contribute to the Rand project<br>Copyright 2018 Developers of the Rand project<br>Copyright (c) 2014 The Rust Project Developers |
| [ref-cast-impl](https://github.com/dtolnay/ref-cast) | 1.0.27 | MIT OR Apache-2.0 | MIT | Copyright the ref-cast-impl authors (David Tolnay) |
| [rust-embed-impl](https://pyrossh.dev/repos/rust-embed) | 8.12.0 | MIT | MIT | Copyright (c) 2018 pyros2097 |
| [rust-i18n-macro](https://github.com/longbridge/rust-i18n) | 3.1.5 | MIT | MIT | Copyright the rust-i18n-macro authors |
| [rustc-hash](https://github.com/rust-lang-nursery/rustc-hash) | 1.1.0 | Apache-2.0/MIT | MIT | Copyright the rustc-hash authors (The Rust Project Developers) |
| [rustc_version](https://github.com/djc/rustc-version-rs) | 0.4.1 | MIT OR Apache-2.0 | MIT | Copyright (c) 2016 The Rust Project Developers |
| [rustversion](https://github.com/dtolnay/rustversion) | 1.0.23 | MIT OR Apache-2.0 | MIT | Copyright the rustversion authors (David Tolnay) |
| [schemars_derive](https://github.com/GREsau/schemars) | 1.2.2 | MIT | MIT | Copyright (c) 2019 Graham Esau |
| [semver](https://github.com/dtolnay/semver) | 1.0.28 | MIT OR Apache-2.0 | MIT | Copyright the semver authors (David Tolnay) |
| [serde_derive](https://github.com/serde-rs/serde) | 1.0.229 | MIT OR Apache-2.0 | MIT | Copyright the serde_derive authors (Erick Tryzelaar, David Tolnay) |
| [serde_derive_internals](https://github.com/serde-rs/serde) | 0.30.0 | MIT OR Apache-2.0 | MIT | Copyright the serde_derive_internals authors (Erick Tryzelaar, David Tolnay) |
| [serde_repr](https://github.com/dtolnay/serde-repr) | 0.1.21 | MIT OR Apache-2.0 | MIT | Copyright the serde_repr authors (David Tolnay) |
| [shellexpand](https://gitlab.com/ijackson/rust-shellexpand) | 3.1.2 | MIT/Apache-2.0 | MIT | Copyright (c) 2016 Vladimir Matveev |
| [shlex](https://github.com/comex/rust-shlex) | 2.0.1 | MIT OR Apache-2.0 | MIT | Copyright 2015 Nicholas Allegra (comex)<br>Copyright (c) 2015 Nicholas Allegra (comex) |
| [simd_helpers](https://github.com/lu-zero/simd_helpers) | 0.1.0 | MIT | MIT | Copyright the simd_helpers authors (Luca Barbato) |
| [stacksafe-macro](https://github.com/fast/stacksafe) | 0.1.4 | Apache-2.0 | Apache-2.0 | Copyright the stacksafe-macro authors |
| [string_cache_codegen](https://github.com/servo/string-cache) | 0.5.4 | MIT OR Apache-2.0 | MIT | Copyright (c) 2012-2013 Mozilla Foundation |
| [strum](https://github.com/Peternator7/strum) | 0.26.3 | MIT | MIT | Copyright (c) 2019 Peter Glotfelty |
| [strum_macros](https://github.com/Peternator7/strum) | 0.26.4 | MIT | MIT | Copyright (c) 2019 Peter Glotfelty |
| [strum_macros](https://github.com/Peternator7/strum) | 0.27.2 | MIT | MIT | Copyright (c) 2019 Peter Glotfelty |
| [syn](https://github.com/dtolnay/syn) | 2.0.119 | MIT OR Apache-2.0 | MIT | Copyright the syn authors (David Tolnay) |
| [syn](https://github.com/dtolnay/syn) | 3.0.6 | MIT OR Apache-2.0 | MIT | Copyright the syn authors (David Tolnay) |
| [synstructure](https://github.com/mystor/synstructure) | 0.14.0 | MIT | MIT | Copyright 2016 Nika Layzell |
| [termcolor](https://github.com/BurntSushi/termcolor) | 1.4.1 | Unlicense OR MIT | MIT | Copyright (c) 2015 Andrew Gallant |
| [thiserror-impl](https://github.com/dtolnay/thiserror) | 1.0.69 | MIT OR Apache-2.0 | MIT | Copyright the thiserror-impl authors (David Tolnay) |
| [thiserror-impl](https://github.com/dtolnay/thiserror) | 2.0.21 | MIT OR Apache-2.0 | MIT | Copyright the thiserror-impl authors (David Tolnay) |
| [time-macros](https://github.com/time-rs/time) | 0.2.32 | MIT OR Apache-2.0 | MIT | Copyright (c) Jacob Pratt et al |
| [tokio-macros](https://github.com/tokio-rs/tokio) | 2.7.2 | MIT | MIT | Copyright (c) 2019 Yoshua Wuyts<br>Copyright (c) Tokio Contributors |
| [tracing-attributes](https://github.com/tokio-rs/tracing) | 0.1.31 | MIT | MIT | Copyright (c) 2019 Tokio Contributors |
| [unicode-width](https://github.com/unicode-rs/unicode-width) | 0.2.2 | MIT OR Apache-2.0 | MIT | Copyright (c) 2015 The Rust Project Developers |
| [vcpkg](https://github.com/mcgoo/vcpkg-rs) | 0.2.15 | MIT/Apache-2.0 | MIT | Copyright (c) 2017 Jim McGrath |
| [version_check](https://github.com/SergioBenitez/version_check) | 0.9.5 | MIT/Apache-2.0 | MIT | Copyright (c) 2017-2018 Sergio Benitez |
| [wasm-bindgen-macro](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro) | 0.2.129 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [wasm-bindgen-macro-support](https://github.com/wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support) | 0.2.129 | MIT OR Apache-2.0 | MIT | Copyright (c) 2014 Alex Crichton |
| [yoke-derive](https://github.com/unicode-org/icu4x) | 0.8.4 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [zerocopy-derive](https://github.com/google/zerocopy) | 0.8.59 | BSD-2-Clause OR Apache-2.0 OR MIT | MIT | Copyright 2023 The Fuchsia Authors<br>Copyright 2019 The Fuchsia Authors |
| [zerofrom-derive](https://github.com/unicode-org/icu4x) | 0.1.8 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |
| [zeroize_derive](https://github.com/RustCrypto/utils) | 1.5.0 | Apache-2.0 OR MIT | MIT | Copyright (c) 2019-2026 The RustCrypto Project Developers |
| [zerovec-derive](https://github.com/unicode-org/icu4x) | 0.11.6 | Unicode-3.0 | Unicode-3.0 | Copyright © 2020-2024 Unicode, Inc |

### Licence texts

#### MIT

```
Copyright (c) <the copyright holders named for each crate above>

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
```

#### Apache-2.0

```
Apache License
                        Version 2.0, January 2004
                     https://www.apache.org/licenses/LICENSE-2.0

TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

1. Definitions.

   "License" shall mean the terms and conditions for use, reproduction,
   and distribution as defined by Sections 1 through 9 of this document.

   "Licensor" shall mean the copyright owner or entity authorized by
   the copyright owner that is granting the License.

   "Legal Entity" shall mean the union of the acting entity and all
   other entities that control, are controlled by, or are under common
   control with that entity. For the purposes of this definition,
   "control" means (i) the power, direct or indirect, to cause the
   direction or management of such entity, whether by contract or
   otherwise, or (ii) ownership of fifty percent (50%) or more of the
   outstanding shares, or (iii) beneficial ownership of such entity.

   "You" (or "Your") shall mean an individual or Legal Entity
   exercising permissions granted by this License.

   "Source" form shall mean the preferred form for making modifications,
   including but not limited to software source code, documentation
   source, and configuration files.

   "Object" form shall mean any form resulting from mechanical
   transformation or translation of a Source form, including but
   not limited to compiled object code, generated documentation,
   and conversions to other media types.

   "Work" shall mean the work of authorship, whether in Source or
   Object form, made available under the License, as indicated by a
   copyright notice that is included in or attached to the work
   (an example is provided in the Appendix below).

   "Derivative Works" shall mean any work, whether in Source or Object
   form, that is based on (or derived from) the Work and for which the
   editorial revisions, annotations, elaborations, or other modifications
   represent, as a whole, an original work of authorship. For the purposes
   of this License, Derivative Works shall not include works that remain
   separable from, or merely link (or bind by name) to the interfaces of,
   the Work and Derivative Works thereof.

   "Contribution" shall mean any work of authorship, including
   the original version of the Work and any modifications or additions
   to that Work or Derivative Works thereof, that is intentionally
   submitted to Licensor for inclusion in the Work by the copyright owner
   or by an individual or Legal Entity authorized to submit on behalf of
   the copyright owner. For the purposes of this definition, "submitted"
   means any form of electronic, verbal, or written communication sent
   to the Licensor or its representatives, including but not limited to
   communication on electronic mailing lists, source code control systems,
   and issue tracking systems that are managed by, or on behalf of, the
   Licensor for the purpose of discussing and improving the Work, but
   excluding communication that is conspicuously marked or otherwise
   designated in writing by the copyright owner as "Not a Contribution."

   "Contributor" shall mean Licensor and any individual or Legal Entity
   on behalf of whom a Contribution has been received by Licensor and
   subsequently incorporated within the Work.

2. Grant of Copyright License. Subject to the terms and conditions of
   this License, each Contributor hereby grants to You a perpetual,
   worldwide, non-exclusive, no-charge, royalty-free, irrevocable
   copyright license to reproduce, prepare Derivative Works of,
   publicly display, publicly perform, sublicense, and distribute the
   Work and such Derivative Works in Source or Object form.

3. Grant of Patent License. Subject to the terms and conditions of
   this License, each Contributor hereby grants to You a perpetual,
   worldwide, non-exclusive, no-charge, royalty-free, irrevocable
   (except as stated in this section) patent license to make, have made,
   use, offer to sell, sell, import, and otherwise transfer the Work,
   where such license applies only to those patent claims licensable
   by such Contributor that are necessarily infringed by their
   Contribution(s) alone or by combination of their Contribution(s)
   with the Work to which such Contribution(s) was submitted. If You
   institute patent litigation against any entity (including a
   cross-claim or counterclaim in a lawsuit) alleging that the Work
   or a Contribution incorporated within the Work constitutes direct
   or contributory patent infringement, then any patent licenses
   granted to You under this License for that Work shall terminate
   as of the date such litigation is filed.

4. Redistribution. You may reproduce and distribute copies of the
   Work or Derivative Works thereof in any medium, with or without
   modifications, and in Source or Object form, provided that You
   meet the following conditions:

   (a) You must give any other recipients of the Work or
       Derivative Works a copy of this License; and

   (b) You must cause any modified files to carry prominent notices
       stating that You changed the files; and

   (c) You must retain, in the Source form of any Derivative Works
       that You distribute, all copyright, patent, trademark, and
       attribution notices from the Source form of the Work,
       excluding those notices that do not pertain to any part of
       the Derivative Works; and

   (d) If the Work includes a "NOTICE" text file as part of its
       distribution, then any Derivative Works that You distribute must
       include a readable copy of the attribution notices contained
       within such NOTICE file, excluding those notices that do not
       pertain to any part of the Derivative Works, in at least one
       of the following places: within a NOTICE text file distributed
       as part of the Derivative Works; within the Source form or
       documentation, if provided along with the Derivative Works; or,
       within a display generated by the Derivative Works, if and
       wherever such third-party notices normally appear. The contents
       of the NOTICE file are for informational purposes only and
       do not modify the License. You may add Your own attribution
       notices within Derivative Works that You distribute, alongside
       or as an addendum to the NOTICE text from the Work, provided
       that such additional attribution notices cannot be construed
       as modifying the License.

   You may add Your own copyright statement to Your modifications and
   may provide additional or different license terms and conditions
   for use, reproduction, or distribution of Your modifications, or
   for any such Derivative Works as a whole, provided Your use,
   reproduction, and distribution of the Work otherwise complies with
   the conditions stated in this License.

5. Submission of Contributions. Unless You explicitly state otherwise,
   any Contribution intentionally submitted for inclusion in the Work
   by You to the Licensor shall be under the terms and conditions of
   this License, without any additional terms or conditions.
   Notwithstanding the above, nothing herein shall supersede or modify
   the terms of any separate license agreement you may have executed
   with Licensor regarding such Contributions.

6. Trademarks. This License does not grant permission to use the trade
   names, trademarks, service marks, or product names of the Licensor,
   except as required for reasonable and customary use in describing the
   origin of the Work and reproducing the content of the NOTICE file.

7. Disclaimer of Warranty. Unless required by applicable law or
   agreed to in writing, Licensor provides the Work (and each
   Contributor provides its Contributions) on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
   implied, including, without limitation, any warranties or conditions
   of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
   PARTICULAR PURPOSE. You are solely responsible for determining the
   appropriateness of using or redistributing the Work and assume any
   risks associated with Your exercise of permissions under this License.

8. Limitation of Liability. In no event and under no legal theory,
   whether in tort (including negligence), contract, or otherwise,
   unless required by applicable law (such as deliberate and grossly
   negligent acts) or agreed to in writing, shall any Contributor be
   liable to You for damages, including any direct, indirect, special,
   incidental, or consequential damages of any character arising as a
   result of this License or out of the use or inability to use the
   Work (including but not limited to damages for loss of goodwill,
   work stoppage, computer failure or malfunction, or any and all
   other commercial damages or losses), even if such Contributor
   has been advised of the possibility of such damages.

9. Accepting Warranty or Additional Liability. While redistributing
   the Work or Derivative Works thereof, You may choose to offer,
   and charge a fee for, acceptance of support, warranty, indemnity,
   or other liability obligations and/or rights consistent with this
   License. However, in accepting such obligations, You may act only
   on Your own behalf and on Your sole responsibility, not on behalf
   of any other Contributor, and only if You agree to indemnify,
   defend, and hold each Contributor harmless for any liability
   incurred by, or claims asserted against, such Contributor by reason
   of your accepting any such warranty or additional liability.

END OF TERMS AND CONDITIONS

APPENDIX: How to apply the Apache License to your work.

   To apply the Apache License to your work, attach the following
   boilerplate notice, with the fields enclosed by brackets "[]"
   replaced with your own identifying information. (Don't include
   the brackets!)  The text should be enclosed in the appropriate
   comment syntax for the file format. We also recommend that a
   file or class name and description of purpose be included on the
   same "printed page" as the copyright notice for easier
   identification within third-party archives.

Copyright [yyyy] [name of copyright owner]

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

	https://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
```

#### BSD-3-Clause

```
Copyright (c) <the copyright holders named for each crate above>

BSD 3-Clause License

All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

#### BSD-2-Clause

```
Copyright (c) <the copyright holders named for each crate above>

All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in the
   documentation and/or other materials provided with the
   distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

#### ISC

```
Copyright (c) <the copyright holders named for each crate above>

Permission to use, copy, modify, and/or distribute this software for any purpose with or without
fee is hereby granted, provided that the above copyright notice and this permission notice appear
in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF
THIS SOFTWARE.
```

#### Zlib

```
Copyright (c) <the copyright holders named for each crate above>

This software is provided 'as-is', without any express or implied warranty. In no event will the authors be held liable for any damages arising from the use of this software.

Permission is granted to anyone to use this software for any purpose, including commercial applications, and to alter it and redistribute it freely, subject to the following restrictions:

1. The origin of this software must not be misrepresented; you must not claim that you wrote the original software. If you use this software in a product, an acknowledgment in the product documentation would be appreciated but is not required.

2. Altered source versions must be plainly marked as such, and must not be misrepresented as being the original software.

3. This notice may not be removed or altered from any source distribution.
```

#### 0BSD

```
Copyright (c) <the copyright holders named for each crate above>

BSD Zero Clause License

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH
REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY
AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT,
INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM
LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR
OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR
PERFORMANCE OF THIS SOFTWARE.
```

#### CC0-1.0

```
Creative Commons Legal Code

CC0 1.0 Universal

    CREATIVE COMMONS CORPORATION IS NOT A LAW FIRM AND DOES NOT PROVIDE
    LEGAL SERVICES. DISTRIBUTION OF THIS DOCUMENT DOES NOT CREATE AN
    ATTORNEY-CLIENT RELATIONSHIP. CREATIVE COMMONS PROVIDES THIS
    INFORMATION ON AN "AS-IS" BASIS. CREATIVE COMMONS MAKES NO WARRANTIES
    REGARDING THE USE OF THIS DOCUMENT OR THE INFORMATION OR WORKS
    PROVIDED HEREUNDER, AND DISCLAIMS LIABILITY FOR DAMAGES RESULTING FROM
    THE USE OF THIS DOCUMENT OR THE INFORMATION OR WORKS PROVIDED
    HEREUNDER.

Statement of Purpose

The laws of most jurisdictions throughout the world automatically confer
exclusive Copyright and Related Rights (defined below) upon the creator
and subsequent owner(s) (each and all, an "owner") of an original work of
authorship and/or a database (each, a "Work").

Certain owners wish to permanently relinquish those rights to a Work for
the purpose of contributing to a commons of creative, cultural and
scientific works ("Commons") that the public can reliably and without fear
of later claims of infringement build upon, modify, incorporate in other
works, reuse and redistribute as freely as possible in any form whatsoever
and for any purposes, including without limitation commercial purposes.
These owners may contribute to the Commons to promote the ideal of a free
culture and the further production of creative, cultural and scientific
works, or to gain reputation or greater distribution for their Work in
part through the use and efforts of others.

For these and/or other purposes and motivations, and without any
expectation of additional consideration or compensation, the person
associating CC0 with a Work (the "Affirmer"), to the extent that he or she
is an owner of Copyright and Related Rights in the Work, voluntarily
elects to apply CC0 to the Work and publicly distribute the Work under its
terms, with knowledge of his or her Copyright and Related Rights in the
Work and the meaning and intended legal effect of CC0 on those rights.

1. Copyright and Related Rights. A Work made available under CC0 may be
protected by copyright and related or neighboring rights ("Copyright and
Related Rights"). Copyright and Related Rights include, but are not
limited to, the following:

  i. the right to reproduce, adapt, distribute, perform, display,
     communicate, and translate a Work;
 ii. moral rights retained by the original author(s) and/or performer(s);
iii. publicity and privacy rights pertaining to a person's image or
     likeness depicted in a Work;
 iv. rights protecting against unfair competition in regards to a Work,
     subject to the limitations in paragraph 4(a), below;
  v. rights protecting the extraction, dissemination, use and reuse of data
     in a Work;
 vi. database rights (such as those arising under Directive 96/9/EC of the
     European Parliament and of the Council of 11 March 1996 on the legal
     protection of databases, and under any national implementation
     thereof, including any amended or successor version of such
     directive); and
vii. other similar, equivalent or corresponding rights throughout the
     world based on applicable law or treaty, and any national
     implementations thereof.

2. Waiver. To the greatest extent permitted by, but not in contravention
of, applicable law, Affirmer hereby overtly, fully, permanently,
irrevocably and unconditionally waives, abandons, and surrenders all of
Affirmer's Copyright and Related Rights and associated claims and causes
of action, whether now known or unknown (including existing as well as
future claims and causes of action), in the Work (i) in all territories
worldwide, (ii) for the maximum duration provided by applicable law or
treaty (including future time extensions), (iii) in any current or future
medium and for any number of copies, and (iv) for any purpose whatsoever,
including without limitation commercial, advertising or promotional
purposes (the "Waiver"). Affirmer makes the Waiver for the benefit of each
member of the public at large and to the detriment of Affirmer's heirs and
successors, fully intending that such Waiver shall not be subject to
revocation, rescission, cancellation, termination, or any other legal or
equitable action to disrupt the quiet enjoyment of the Work by the public
as contemplated by Affirmer's express Statement of Purpose.

3. Public License Fallback. Should any part of the Waiver for any reason
be judged legally invalid or ineffective under applicable law, then the
Waiver shall be preserved to the maximum extent permitted taking into
account Affirmer's express Statement of Purpose. In addition, to the
extent the Waiver is so judged Affirmer hereby grants to each affected
person a royalty-free, non transferable, non sublicensable, non exclusive,
irrevocable and unconditional license to exercise Affirmer's Copyright and
Related Rights in the Work (i) in all territories worldwide, (ii) for the
maximum duration provided by applicable law or treaty (including future
time extensions), (iii) in any current or future medium and for any number
of copies, and (iv) for any purpose whatsoever, including without
limitation commercial, advertising or promotional purposes (the
"License"). The License shall be deemed effective as of the date CC0 was
applied by Affirmer to the Work. Should any part of the License for any
reason be judged legally invalid or ineffective under applicable law, such
partial invalidity or ineffectiveness shall not invalidate the remainder
of the License, and in such case Affirmer hereby affirms that he or she
will not (i) exercise any of his or her remaining Copyright and Related
Rights in the Work or (ii) assert any associated claims and causes of
action with respect to the Work, in either case contrary to Affirmer's
express Statement of Purpose.

4. Limitations and Disclaimers.

 a. No trademark or patent rights held by Affirmer are waived, abandoned,
    surrendered, licensed or otherwise affected by this document.
 b. Affirmer offers the Work as-is and makes no representations or
    warranties of any kind concerning the Work, express, implied,
    statutory or otherwise, including without limitation warranties of
    title, merchantability, fitness for a particular purpose, non
    infringement, or the absence of latent or other defects, accuracy, or
    the present or absence of errors, whether or not discoverable, all to
    the greatest extent permissible under applicable law.
 c. Affirmer disclaims responsibility for clearing rights of other persons
    that may apply to the Work or any use thereof, including without
    limitation any person's Copyright and Related Rights in the Work.
    Further, Affirmer disclaims responsibility for obtaining any necessary
    consents, permissions or other rights required for any use of the
    Work.
 d. Affirmer understands and acknowledges that Creative Commons is not a
    party to this document and has no duty or obligation with respect to
    this CC0 or use of the Work.
```

#### Unicode-3.0

```
UNICODE LICENSE V3

COPYRIGHT AND PERMISSION NOTICE

Copyright © 2020-2024 Unicode, Inc.

NOTICE TO USER: Carefully read the following legal agreement. BY
DOWNLOADING, INSTALLING, COPYING OR OTHERWISE USING DATA FILES, AND/OR
SOFTWARE, YOU UNEQUIVOCALLY ACCEPT, AND AGREE TO BE BOUND BY, ALL OF THE
TERMS AND CONDITIONS OF THIS AGREEMENT. IF YOU DO NOT AGREE, DO NOT
DOWNLOAD, INSTALL, COPY, DISTRIBUTE OR USE THE DATA FILES OR SOFTWARE.

Permission is hereby granted, free of charge, to any person obtaining a
copy of data files and any associated documentation (the "Data Files") or
software and any associated documentation (the "Software") to deal in the
Data Files or Software without restriction, including without limitation
the rights to use, copy, modify, merge, publish, distribute, and/or sell
copies of the Data Files or Software, and to permit persons to whom the
Data Files or Software are furnished to do so, provided that either (a)
this copyright and permission notice appear with all copies of the Data
Files or Software, or (b) this copyright and permission notice appear in
associated Documentation.

THE DATA FILES AND SOFTWARE ARE PROVIDED "AS IS", WITHOUT WARRANTY OF ANY
KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF
THIRD PARTY RIGHTS.

IN NO EVENT SHALL THE COPYRIGHT HOLDER OR HOLDERS INCLUDED IN THIS NOTICE
BE LIABLE FOR ANY CLAIM, OR ANY SPECIAL INDIRECT OR CONSEQUENTIAL DAMAGES,
OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THE DATA
FILES OR SOFTWARE.

Except as contained in this notice, the name of a copyright holder shall
not be used in advertising or otherwise to promote the sale, use or other
dealings in these Data Files or Software without prior written
authorization of the copyright holder.

SPDX-License-Identifier: Unicode-3.0

—

Portions of ICU4X may have been adapted from ICU4C and/or ICU4J.
ICU 1.8.1 to ICU 57.1 © 1995-2016 International Business Machines Corporation and others.
```

#### CDLA-Permissive-2.0

```
# Community Data License Agreement - Permissive - Version 2.0

This is the Community Data License Agreement - Permissive, Version
2.0 (the "agreement"). Data Provider(s) and Data Recipient(s) agree
as follows:

## 1. Provision of the Data

1.1. A Data Recipient may use, modify, and share the Data made
available by Data Provider(s) under this agreement if that Data
Recipient follows the terms of this agreement.

1.2. This agreement does not impose any restriction on a Data
Recipient's use, modification, or sharing of any portions of the
Data that are in the public domain or that may be used, modified,
or shared under any other legal exception or limitation.

## 2. Conditions for Sharing Data

2.1. A Data Recipient may share Data, with or without modifications, so
long as the Data Recipient makes available the text of this agreement
with the shared Data.

## 3. No Restrictions on Results

3.1. This agreement does not impose any restriction or obligations
with respect to the use, modification, or sharing of Results.

## 4. No Warranty; Limitation of Liability

4.1. All Data Recipients receive the Data subject to the following
terms:

THE DATA IS PROVIDED ON AN "AS IS" BASIS, WITHOUT REPRESENTATIONS,
WARRANTIES OR CONDITIONS OF ANY KIND, EITHER EXPRESS OR IMPLIED
INCLUDING, WITHOUT LIMITATION, ANY WARRANTIES OR CONDITIONS OF TITLE,
NON-INFRINGEMENT, MERCHANTABILITY OR FITNESS FOR A PARTICULAR PURPOSE.

NO DATA PROVIDER SHALL HAVE ANY LIABILITY FOR ANY DIRECT, INDIRECT,
INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING
WITHOUT LIMITATION LOST PROFITS), HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE DATA OR RESULTS,
EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGES.

## 5. Definitions

5.1. "Data" means the material received by a Data Recipient under
this agreement.

5.2. "Data Provider" means any person who is the source of Data
provided under this agreement and in reliance on a Data Recipient's
agreement to its terms.

5.3. "Data Recipient" means any person who receives Data directly
or indirectly from a Data Provider and agrees to the terms of this
agreement.

5.4. "Results" means any outcome obtained by computational analysis
of Data, including for example machine learning models and models'
insights.
```

#### Apache-2.0 WITH LLVM-exception

```
Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

    TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

    1. Definitions.

      "License" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.

      "Licensor" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.

      "Legal Entity" shall mean the union of the acting entity and all
      other entities that control, are controlled by, or are under common
      control with that entity. For the purposes of this definition,
      "control" means (i) the power, direct or indirect, to cause the
      direction or management of such entity, whether by contract or
      otherwise, or (ii) ownership of fifty percent (50%) or more of the
      outstanding shares, or (iii) beneficial ownership of such entity.

      "You" (or "Your") shall mean an individual or Legal Entity
      exercising permissions granted by this License.

      "Source" form shall mean the preferred form for making modifications,
      including but not limited to software source code, documentation
      source, and configuration files.

      "Object" form shall mean any form resulting from mechanical
      transformation or translation of a Source form, including but
      not limited to compiled object code, generated documentation,
      and conversions to other media types.

      "Work" shall mean the work of authorship, whether in Source or
      Object form, made available under the License, as indicated by a
      copyright notice that is included in or attached to the work
      (an example is provided in the Appendix below).

      "Derivative Works" shall mean any work, whether in Source or Object
      form, that is based on (or derived from) the Work and for which the
      editorial revisions, annotations, elaborations, or other modifications
      represent, as a whole, an original work of authorship. For the purposes
      of this License, Derivative Works shall not include works that remain
      separable from, or merely link (or bind by name) to the interfaces of,
      the Work and Derivative Works thereof.

      "Contribution" shall mean any work of authorship, including
      the original version of the Work and any modifications or additions
      to that Work or Derivative Works thereof, that is intentionally
      submitted to Licensor for inclusion in the Work by the copyright owner
      or by an individual or Legal Entity authorized to submit on behalf of
      the copyright owner. For the purposes of this definition, "submitted"
      means any form of electronic, verbal, or written communication sent
      to the Licensor or its representatives, including but not limited to
      communication on electronic mailing lists, source code control systems,
      and issue tracking systems that are managed by, or on behalf of, the
      Licensor for the purpose of discussing and improving the Work, but
      excluding communication that is conspicuously marked or otherwise
      designated in writing by the copyright owner as "Not a Contribution."

      "Contributor" shall mean Licensor and any individual or Legal Entity
      on behalf of whom a Contribution has been received by Licensor and
      subsequently incorporated within the Work.

    2. Grant of Copyright License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      copyright license to reproduce, prepare Derivative Works of,
      publicly display, publicly perform, sublicense, and distribute the
      Work and such Derivative Works in Source or Object form.

    3. Grant of Patent License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      (except as stated in this section) patent license to make, have made,
      use, offer to sell, sell, import, and otherwise transfer the Work,
      where such license applies only to those patent claims licensable
      by such Contributor that are necessarily infringed by their
      Contribution(s) alone or by combination of their Contribution(s)
      with the Work to which such Contribution(s) was submitted. If You
      institute patent litigation against any entity (including a
      cross-claim or counterclaim in a lawsuit) alleging that the Work
      or a Contribution incorporated within the Work constitutes direct
      or contributory patent infringement, then any patent licenses
      granted to You under this License for that Work shall terminate
      as of the date such litigation is filed.

    4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:

      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and

      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and

      (c) You must retain, in the Source form of any Derivative Works
          that You distribute, all copyright, patent, trademark, and
          attribution notices from the Source form of the Work,
          excluding those notices that do not pertain to any part of
          the Derivative Works; and

      (d) If the Work includes a "NOTICE" text file as part of its
          distribution, then any Derivative Works that You distribute must
          include a readable copy of the attribution notices contained
          within such NOTICE file, excluding those notices that do not
          pertain to any part of the Derivative Works, in at least one
          of the following places: within a NOTICE text file distributed
          as part of the Derivative Works; within the Source form or
          documentation, if provided along with the Derivative Works; or,
          within a display generated by the Derivative Works, if and
          wherever such third-party notices normally appear. The contents
          of the NOTICE file are for informational purposes only and
          do not modify the License. You may add Your own attribution
          notices within Derivative Works that You distribute, alongside
          or as an addendum to the NOTICE text from the Work, provided
          that such additional attribution notices cannot be construed
          as modifying the License.

      You may add Your own copyright statement to Your modifications and
      may provide additional or different license terms and conditions
      for use, reproduction, or distribution of Your modifications, or
      for any such Derivative Works as a whole, provided Your use,
      reproduction, and distribution of the Work otherwise complies with
      the conditions stated in this License.

    5. Submission of Contributions. Unless You explicitly state otherwise,
      any Contribution intentionally submitted for inclusion in the Work
      by You to the Licensor shall be under the terms and conditions of
      this License, without any additional terms or conditions.
      Notwithstanding the above, nothing herein shall supersede or modify
      the terms of any separate license agreement you may have executed
      with Licensor regarding such Contributions.

    6. Trademarks. This License does not grant permission to use the trade
      names, trademarks, service marks, or product names of the Licensor,
      except as required for reasonable and customary use in describing the
      origin of the Work and reproducing the content of the NOTICE file.

    7. Disclaimer of Warranty. Unless required by applicable law or
      agreed to in writing, Licensor provides the Work (and each
      Contributor provides its Contributions) on an "AS IS" BASIS,
      WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
      implied, including, without limitation, any warranties or conditions
      of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
      PARTICULAR PURPOSE. You are solely responsible for determining the
      appropriateness of using or redistributing the Work and assume any
      risks associated with Your exercise of permissions under this License.

    8. Limitation of Liability. In no event and under no legal theory,
      whether in tort (including negligence), contract, or otherwise,
      unless required by applicable law (such as deliberate and grossly
      negligent acts) or agreed to in writing, shall any Contributor be
      liable to You for damages, including any direct, indirect, special,
      incidental, or consequential damages of any character arising as a
      result of this License or out of the use or inability to use the
      Work (including but not limited to damages for loss of goodwill,
      work stoppage, computer failure or malfunction, or any and all
      other commercial damages or losses), even if such Contributor
      has been advised of the possibility of such damages.

    9. Accepting Warranty or Additional Liability. While redistributing
      the Work or Derivative Works thereof, You may choose to offer,
      and charge a fee for, acceptance of support, warranty, indemnity,
      or other liability obligations and/or rights consistent with this
      License. However, in accepting such obligations, You may act only
      on Your own behalf and on Your sole responsibility, not on behalf
      of any other Contributor, and only if You agree to indemnify,
      defend, and hold each Contributor harmless for any liability
      incurred by, or claims asserted against, such Contributor by reason
      of your accepting any such warranty or additional liability.

    END OF TERMS AND CONDITIONS

    APPENDIX: How to apply the Apache License to your work.

      To apply the Apache License to your work, attach the following
      boilerplate notice, with the fields enclosed by brackets "[]"
      replaced with your own identifying information. (Don't include
      the brackets!)  The text should be enclosed in the appropriate
      comment syntax for the file format. We also recommend that a
      file or class name and description of purpose be included on the
      same "printed page" as the copyright notice for easier
      identification within third-party archives.

    Copyright [yyyy] [name of copyright owner]

    Licensed under the Apache License, Version 2.0 (the "License");
    you may not use this file except in compliance with the License.
    You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

    Unless required by applicable law or agreed to in writing, software
    distributed under the License is distributed on an "AS IS" BASIS,
    WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
    See the License for the specific language governing permissions and
    limitations under the License.


--- LLVM Exceptions to the Apache 2.0 License ----

As an exception, if, as a result of your compiling your source code, portions
of this Software are embedded into an Object form of such source code, you
may redistribute such embedded portions in such Object form without complying
with the conditions of Sections 4(a), 4(b) and 4(d) of the License.

In addition, if you combine or link compiled forms of this Software with
software that is licensed under the GPLv2 ("Combined Software") and if a
court of competent jurisdiction determines that the patent provision (Section
3), the indemnity provision (Section 9) or other Section of the License
conflicts with the conditions of the GPLv2, you may retroactively and
prospectively choose to deem waived or otherwise exclude such Section(s) of
the License, but only in their entirety and only with respect to the Combined
Software.
```

#### MPL-2.0

```
Mozilla Public License Version 2.0
==================================

1. Definitions
--------------

1.1. "Contributor"
    means each individual or legal entity that creates, contributes to
    the creation of, or owns Covered Software.

1.2. "Contributor Version"
    means the combination of the Contributions of others (if any) used
    by a Contributor and that particular Contributor's Contribution.

1.3. "Contribution"
    means Covered Software of a particular Contributor.

1.4. "Covered Software"
    means Source Code Form to which the initial Contributor has attached
    the notice in Exhibit A, the Executable Form of such Source Code
    Form, and Modifications of such Source Code Form, in each case
    including portions thereof.

1.5. "Incompatible With Secondary Licenses"
    means

    (a) that the initial Contributor has attached the notice described
        in Exhibit B to the Covered Software; or

    (b) that the Covered Software was made available under the terms of
        version 1.1 or earlier of the License, but not also under the
        terms of a Secondary License.

1.6. "Executable Form"
    means any form of the work other than Source Code Form.

1.7. "Larger Work"
    means a work that combines Covered Software with other material, in
    a separate file or files, that is not Covered Software.

1.8. "License"
    means this document.

1.9. "Licensable"
    means having the right to grant, to the maximum extent possible,
    whether at the time of the initial grant or subsequently, any and
    all of the rights conveyed by this License.

1.10. "Modifications"
    means any of the following:

    (a) any file in Source Code Form that results from an addition to,
        deletion from, or modification of the contents of Covered
        Software; or

    (b) any new file in Source Code Form that contains any Covered
        Software.

1.11. "Patent Claims" of a Contributor
    means any patent claim(s), including without limitation, method,
    process, and apparatus claims, in any patent Licensable by such
    Contributor that would be infringed, but for the grant of the
    License, by the making, using, selling, offering for sale, having
    made, import, or transfer of either its Contributions or its
    Contributor Version.

1.12. "Secondary License"
    means either the GNU General Public License, Version 2.0, the GNU
    Lesser General Public License, Version 2.1, the GNU Affero General
    Public License, Version 3.0, or any later versions of those
    licenses.

1.13. "Source Code Form"
    means the form of the work preferred for making modifications.

1.14. "You" (or "Your")
    means an individual or a legal entity exercising rights under this
    License. For legal entities, "You" includes any entity that
    controls, is controlled by, or is under common control with You. For
    purposes of this definition, "control" means (a) the power, direct
    or indirect, to cause the direction or management of such entity,
    whether by contract or otherwise, or (b) ownership of more than
    fifty percent (50%) of the outstanding shares or beneficial
    ownership of such entity.

2. License Grants and Conditions
--------------------------------

2.1. Grants

Each Contributor hereby grants You a world-wide, royalty-free,
non-exclusive license:

(a) under intellectual property rights (other than patent or trademark)
    Licensable by such Contributor to use, reproduce, make available,
    modify, display, perform, distribute, and otherwise exploit its
    Contributions, either on an unmodified basis, with Modifications, or
    as part of a Larger Work; and

(b) under Patent Claims of such Contributor to make, use, sell, offer
    for sale, have made, import, and otherwise transfer either its
    Contributions or its Contributor Version.

2.2. Effective Date

The licenses granted in Section 2.1 with respect to any Contribution
become effective for each Contribution on the date the Contributor first
distributes such Contribution.

2.3. Limitations on Grant Scope

The licenses granted in this Section 2 are the only rights granted under
this License. No additional rights or licenses will be implied from the
distribution or licensing of Covered Software under this License.
Notwithstanding Section 2.1(b) above, no patent license is granted by a
Contributor:

(a) for any code that a Contributor has removed from Covered Software;
    or

(b) for infringements caused by: (i) Your and any other third party's
    modifications of Covered Software, or (ii) the combination of its
    Contributions with other software (except as part of its Contributor
    Version); or

(c) under Patent Claims infringed by Covered Software in the absence of
    its Contributions.

This License does not grant any rights in the trademarks, service marks,
or logos of any Contributor (except as may be necessary to comply with
the notice requirements in Section 3.4).

2.4. Subsequent Licenses

No Contributor makes additional grants as a result of Your choice to
distribute the Covered Software under a subsequent version of this
License (see Section 10.2) or under the terms of a Secondary License (if
permitted under the terms of Section 3.3).

2.5. Representation

Each Contributor represents that the Contributor believes its
Contributions are its original creation(s) or it has sufficient rights
to grant the rights to its Contributions conveyed by this License.

2.6. Fair Use

This License is not intended to limit any rights You have under
applicable copyright doctrines of fair use, fair dealing, or other
equivalents.

2.7. Conditions

Sections 3.1, 3.2, 3.3, and 3.4 are conditions of the licenses granted
in Section 2.1.

3. Responsibilities
-------------------

3.1. Distribution of Source Form

All distribution of Covered Software in Source Code Form, including any
Modifications that You create or to which You contribute, must be under
the terms of this License. You must inform recipients that the Source
Code Form of the Covered Software is governed by the terms of this
License, and how they can obtain a copy of this License. You may not
attempt to alter or restrict the recipients' rights in the Source Code
Form.

3.2. Distribution of Executable Form

If You distribute Covered Software in Executable Form then:

(a) such Covered Software must also be made available in Source Code
    Form, as described in Section 3.1, and You must inform recipients of
    the Executable Form how they can obtain a copy of such Source Code
    Form by reasonable means in a timely manner, at a charge no more
    than the cost of distribution to the recipient; and

(b) You may distribute such Executable Form under the terms of this
    License, or sublicense it under different terms, provided that the
    license for the Executable Form does not attempt to limit or alter
    the recipients' rights in the Source Code Form under this License.

3.3. Distribution of a Larger Work

You may create and distribute a Larger Work under terms of Your choice,
provided that You also comply with the requirements of this License for
the Covered Software. If the Larger Work is a combination of Covered
Software with a work governed by one or more Secondary Licenses, and the
Covered Software is not Incompatible With Secondary Licenses, this
License permits You to additionally distribute such Covered Software
under the terms of such Secondary License(s), so that the recipient of
the Larger Work may, at their option, further distribute the Covered
Software under the terms of either this License or such Secondary
License(s).

3.4. Notices

You may not remove or alter the substance of any license notices
(including copyright notices, patent notices, disclaimers of warranty,
or limitations of liability) contained within the Source Code Form of
the Covered Software, except that You may alter any license notices to
the extent required to remedy known factual inaccuracies.

3.5. Application of Additional Terms

You may choose to offer, and to charge a fee for, warranty, support,
indemnity or liability obligations to one or more recipients of Covered
Software. However, You may do so only on Your own behalf, and not on
behalf of any Contributor. You must make it absolutely clear that any
such warranty, support, indemnity, or liability obligation is offered by
You alone, and You hereby agree to indemnify every Contributor for any
liability incurred by such Contributor as a result of warranty, support,
indemnity or liability terms You offer. You may include additional
disclaimers of warranty and limitations of liability specific to any
jurisdiction.

4. Inability to Comply Due to Statute or Regulation
---------------------------------------------------

If it is impossible for You to comply with any of the terms of this
License with respect to some or all of the Covered Software due to
statute, judicial order, or regulation then You must: (a) comply with
the terms of this License to the maximum extent possible; and (b)
describe the limitations and the code they affect. Such description must
be placed in a text file included with all distributions of the Covered
Software under this License. Except to the extent prohibited by statute
or regulation, such description must be sufficiently detailed for a
recipient of ordinary skill to be able to understand it.

5. Termination
--------------

5.1. The rights granted under this License will terminate automatically
if You fail to comply with any of its terms. However, if You become
compliant, then the rights granted under this License from a particular
Contributor are reinstated (a) provisionally, unless and until such
Contributor explicitly and finally terminates Your grants, and (b) on an
ongoing basis, if such Contributor fails to notify You of the
non-compliance by some reasonable means prior to 60 days after You have
come back into compliance. Moreover, Your grants from a particular
Contributor are reinstated on an ongoing basis if such Contributor
notifies You of the non-compliance by some reasonable means, this is the
first time You have received notice of non-compliance with this License
from such Contributor, and You become compliant prior to 30 days after
Your receipt of the notice.

5.2. If You initiate litigation against any entity by asserting a patent
infringement claim (excluding declaratory judgment actions,
counter-claims, and cross-claims) alleging that a Contributor Version
directly or indirectly infringes any patent, then the rights granted to
You by any and all Contributors for the Covered Software under Section
2.1 of this License shall terminate.

5.3. In the event of termination under Sections 5.1 or 5.2 above, all
end user license agreements (excluding distributors and resellers) which
have been validly granted by You or Your distributors under this License
prior to termination shall survive termination.

************************************************************************
*                                                                      *
*  6. Disclaimer of Warranty                                           *
*  -------------------------                                           *
*                                                                      *
*  Covered Software is provided under this License on an "as is"       *
*  basis, without warranty of any kind, either expressed, implied, or  *
*  statutory, including, without limitation, warranties that the       *
*  Covered Software is free of defects, merchantable, fit for a        *
*  particular purpose or non-infringing. The entire risk as to the     *
*  quality and performance of the Covered Software is with You.        *
*  Should any Covered Software prove defective in any respect, You     *
*  (not any Contributor) assume the cost of any necessary servicing,   *
*  repair, or correction. This disclaimer of warranty constitutes an   *
*  essential part of this License. No use of any Covered Software is   *
*  authorized under this License except under this disclaimer.         *
*                                                                      *
************************************************************************

************************************************************************
*                                                                      *
*  7. Limitation of Liability                                          *
*  --------------------------                                          *
*                                                                      *
*  Under no circumstances and under no legal theory, whether tort      *
*  (including negligence), contract, or otherwise, shall any           *
*  Contributor, or anyone who distributes Covered Software as          *
*  permitted above, be liable to You for any direct, indirect,         *
*  special, incidental, or consequential damages of any character      *
*  including, without limitation, damages for lost profits, loss of    *
*  goodwill, work stoppage, computer failure or malfunction, or any    *
*  and all other commercial damages or losses, even if such party      *
*  shall have been informed of the possibility of such damages. This   *
*  limitation of liability shall not apply to liability for death or   *
*  personal injury resulting from such party's negligence to the       *
*  extent applicable law prohibits such limitation. Some               *
*  jurisdictions do not allow the exclusion or limitation of           *
*  incidental or consequential damages, so this exclusion and          *
*  limitation may not apply to You.                                    *
*                                                                      *
************************************************************************

8. Litigation
-------------

Any litigation relating to this License may be brought only in the
courts of a jurisdiction where the defendant maintains its principal
place of business and such litigation shall be governed by laws of that
jurisdiction, without reference to its conflict-of-law provisions.
Nothing in this Section shall prevent a party's ability to bring
cross-claims or counter-claims.

9. Miscellaneous
----------------

This License represents the complete agreement concerning the subject
matter hereof. If any provision of this License is held to be
unenforceable, such provision shall be reformed only to the extent
necessary to make it enforceable. Any law or regulation which provides
that the language of a contract shall be construed against the drafter
shall not be used to construe this License against a Contributor.

10. Versions of the License
---------------------------

10.1. New Versions

Mozilla Foundation is the license steward. Except as provided in Section
10.3, no one other than the license steward has the right to modify or
publish new versions of this License. Each version will be given a
distinguishing version number.

10.2. Effect of New Versions

You may distribute the Covered Software under the terms of the version
of the License under which You originally received the Covered Software,
or under the terms of any subsequent version published by the license
steward.

10.3. Modified Versions

If you create software not governed by this License, and you want to
create a new license for such software, you may create and use a
modified version of this License if you rename the license and remove
any references to the name of the license steward (except to note that
such modified license differs from this License).

10.4. Distributing Source Code Form that is Incompatible With Secondary
Licenses

If You choose to distribute Source Code Form that is Incompatible With
Secondary Licenses under the terms of this version of the License, the
notice described in Exhibit B of this License must be attached.

Exhibit A - Source Code Form License Notice
-------------------------------------------

  This Source Code Form is subject to the terms of the Mozilla Public
  License, v. 2.0. If a copy of the MPL was not distributed with this
  file, You can obtain one at http://mozilla.org/MPL/2.0/.

If it is not possible or desirable to put the notice in a particular
file, then You may include the notice in a location (such as a LICENSE
file in a relevant directory) where a recipient would be likely to look
for such a notice.

You may add additional accurate notices of copyright ownership.

Exhibit B - "Incompatible With Secondary Licenses" Notice
---------------------------------------------------------

  This Source Code Form is "Incompatible With Secondary Licenses", as
  defined by the Mozilla Public License, v. 2.0.
```

<!-- END generated -->
