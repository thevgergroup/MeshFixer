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

## Boolean kernel (`mf-boolean`, Phase 1 D1 and D2)

Added 2026-11-10 for the Task 7 kernel qualification (ADR-0002). All are
on the `deny.toml` allowlist; no exception is needed.

| Crate | Licence | Why |
|---|---|---|
| `manifold-rust` =0.19.2 | Apache-2.0 | the boolean kernel (robust engine only), D1 |
| `clipper2-rust` 1.2 | BSL-1.0 | a `manifold-rust` dependency (2D polygon clipping) |
| `dashu-int`, `dashu-ratio`, `dashu-base` 0.5 | MIT OR Apache-2.0 | the kernel's exact rational arithmetic |
| `num-modular` 0.6 | Apache-2.0 | a `dashu` dependency |
| `robust` 1.2 | MIT OR Apache-2.0 | exact predicates for `validate_solid`, D2 |

**Obligations when distributing MeshFixer binaries:** include the Apache-2.0
licence text (already shipped as `LICENSE-APACHE`). `manifold-rust` 0.19.2
has no `NOTICE` file. BSL-1.0 asks for its text only in source
distributions, which `cargo vendor` keeps.
