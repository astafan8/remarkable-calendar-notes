# Embedded UI font

`JetBrainsMono-Regular.ttf` is a subset (basic Latin + Latin-1 + a few
punctuation marks) of **JetBrains Mono**, used to render text in the
in-app Settings ("SET") menu — where you type emails, Apple IDs, and
passwords and need real mixed-case, monospaced glyphs. The rest of the app
(calendar buttons, month names, day numbers) uses the built-in compact
bitmap font in `render.rs`.

The font is embedded directly into the binary (`include_bytes!`) and
rasterized with the pure-Rust [`ab_glyph`](https://crates.io/crates/ab_glyph)
crate — there is no Qt or system font dependency, so the app stays a
self-contained static binary.

- Font: JetBrains Mono, © JetBrains, licensed under the **SIL Open Font
  License 1.1** (see [`LICENSE-JetBrainsMono.txt`](LICENSE-JetBrainsMono.txt)).
- The subset was produced with `fontTools.subset` to keep the binary small;
  it is otherwise unmodified.
