# PDF crates

`pdf-engine` and `pdf-render` are copied from the pdfreader project (its `crates/engine` and
`crates/render`), so the reader can open PDFs on Windows and Android. Improvements made there
are copied over by hand; keep local changes small and note them here.

- `pdf-engine`: opens a PDF and renders any part of a page to pixels, with its text (character
  boxes), links and outline. Built on [hayro](https://github.com/LaurenzV/hayro); the renderer in
  `src/raster` is vendored from it (Apache-2.0 or MIT, licences alongside).
- `pdf-render`: page layout, the tile cache, render threads, page colours (dark, sepia) and a
  text thread for search and selection.

Only the PDF parts are copied: not pdfreader's EPUB, comic, DjVu and picture engines, page
rotation or two-page spreads. Otherwise there are no local changes.
