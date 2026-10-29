# Hadion logo

Two marks, one system.

- **Merge**: the primary logo. Two branches form the H; the crossbar is a merge landing on the main line as a commit node.
- **Tile**: the compact mark. Use it as the app icon, favicon, avatar, or anywhere the Merge mark would be too small.

All SVGs are self-contained: the wordmark is converted to outlines (no font needed) and the commit node is a real transparent hole, so the marks sit on any background.

## Files

| File | Use |
|---|---|
| `svg/hadion-lockup.svg` | Primary logo on dark backgrounds |
| `svg/hadion-lockup-light.svg` | Primary logo on light backgrounds |
| `svg/hadion-lockup-stacked*.svg` | Square-ish spaces (splash, about box) |
| `svg/hadion-mark*.svg` | Mark only: colour on dark, colour on light, white, black |
| `svg/hadion-tile.svg` | Tile icon, green |
| `svg/hadion-tile-ink.svg` | Tile icon, single-colour |
| `svg/hadion-tile-16.svg` | Pixel-aligned Tile for 16 px only |
| `svg/hadion-tile-lockup*.svg` | Tile + wordmark, for tight horizontal spaces |
| `ico/hadion.ico` | Windows app icon, 16 to 256 px (16 px uses the pixel-aligned version) |
| `png/` | Raster copies: marks at 256 to 1024 px, lockups 2000 px wide, Tile at 16 to 1024 px |

## Colour

| Name | Hex | Use |
|---|---|---|
| Ink | `#0F1115` | Dark background, mark on light |
| Surface | `#161A21` | Raised dark panels |
| Paper | `#F4F5F2` | Mark and text on dark |
| Branch green | `#3DDC97` | Accent on dark backgrounds, Tile fill |
| Deep green | `#2C8F66` | Accent on light backgrounds (branch green is too pale there) |

## Type

Wordmark: Geist SemiBold, tracking −3.5%. Geist (SIL Open Font License) also works for UI text; Geist Mono for code and labels.

## Rules

- Keep clear space of at least half the wordmark's cap height on every side (built into the lockup files).
- Use the dark-background versions on dark, light on light. Don't put branch green on light backgrounds.
- Don't recolour individual strokes, stretch, rotate, or redraw the crossbar.
- Below 24 px, use the Tile, not the Merge mark.
