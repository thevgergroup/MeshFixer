# Third-party notices

MeshFixer's own code is licensed under MIT OR Apache-2.0. Dependencies are
checked by `cargo deny check licenses` (see `deny.toml`, MF-ARCH-LEGAL-001).
This file records dependencies that need an exception or carry notice
obligations beyond the usual permissive licences.

## egui bundled fonts (`epaint_default_fonts` 0.37)

**Exception approved:** 2026-11-08, for this crate only.

The desktop app embeds these font files in its binary:

| Font file | Licence | Licence text |
|---|---|---|
| `Hack-Regular.ttf` | MIT (Hack), Bitstream Vera License (Bitstream Vera Sans Mono base), public domain (DejaVu changes) | `licenses/third-party/epaint_default_fonts/Hack-Regular.txt` |
| `NotoEmoji-Regular.ttf` | SIL Open Font License 1.1 | `licenses/third-party/epaint_default_fonts/OFL.txt` |
| `Ubuntu-Light.ttf` | Ubuntu Font Licence 1.0 | `licenses/third-party/epaint_default_fonts/UFL.txt` |
| `emoji-icon-font.ttf`, `egui-icons.ttf` (subset) | MIT | `licenses/third-party/epaint_default_fonts/emoji-icon-font-mit-license.txt` |

The crate's metadata declares `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0`. The
Bitstream Vera licence on Hack's base glyphs comes from Hack's own notice file,
not the crate metadata. It's permissive but has reserved font names.

**Obligations when distributing MeshFixer binaries:**
- Include the four licence texts above with the distribution (for example in an
  installer's licences folder or the About dialog). Tracked as MF-ISSUE-016.
- Don't sell the fonts on their own (OFL, UFL). Bundling them in software is permitted.
- Don't use the reserved font names ("Bitstream", "Vera", "Ubuntu") for modified fonts.
  We don't modify them.

None of these licences place obligations on MeshFixer's own source code.
