<div align="center">

<img src="docs/logo.svg" width="104" alt="The Flax Reader mark, a blue flax flower">

# Flax Reader

**A tiny, fast e-reader for Windows and Android, with Archive of Our Own built in.**

About 10 MB of memory · opens in a tenth of a second · a 6 MB download · no web engine

Your EPUBs and PDFs · free classics · AO3 and the story forums · highlights, notes and a dictionary

[![Version](https://img.shields.io/github/v/release/AgastyaShenoy/flax-reader?color=4a68b0&label=version&style=flat-square)](https://github.com/AgastyaShenoy/flax-reader/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/AgastyaShenoy/flax-reader/total?color=4a68b0&style=flat-square)](https://github.com/AgastyaShenoy/flax-reader/releases)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20Android-4a68b0?style=flat-square)](https://github.com/AgastyaShenoy/flax-reader/releases/latest)
[![Memory: about 10 MB](https://img.shields.io/badge/memory-~10%20MB-2ea44f?style=flat-square)](#-light-and-fast)
[![Starts in 0.12 s](https://img.shields.io/badge/starts%20in-0.12%20s-2ea44f?style=flat-square)](#-light-and-fast)
[![Download: 6.3 MB](https://img.shields.io/badge/download-6.3%20MB-2ea44f?style=flat-square)](#-light-and-fast)
[![Core: Rust](https://img.shields.io/badge/core-Rust-b7410e?style=flat-square)](https://www.rust-lang.org/)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-2ea44f?style=flat-square)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join%20us-5865F2?style=flat-square)](https://discord.gg/3xuJY8UXKu)

<a href="https://github.com/AgastyaShenoy/flax-reader/releases/latest"><img src="https://img.shields.io/badge/Download_for-Windows-4a68b0?style=for-the-badge" alt="Download for Windows"></a>
<a href="https://github.com/AgastyaShenoy/flax-reader/releases/latest"><img src="https://img.shields.io/badge/Download_for-Android-4a68b0?style=for-the-badge&logo=android&logoColor=white" alt="Download for Android"></a>

<br><br>

<img src="docs/screenshots/windows/reader.png" width="92%" alt="Reading Alice's Adventures in Wonderland in two columns, with a yellow and a pink highlight, in the Dark Slate theme">

</div>

<br>

> [!IMPORTANT]
> **The AO3 features haven't been tested against the live site yet.** Archive of Our Own has been down for maintenance while they were built, so searching AO3, following authors and tags, downloading works, signing in and leaving kudos were only tested against saved pages. They'll be tested, and fixed where needed, once AO3 is back online. Everything else works now, including reading the AO3 works you've already downloaded.

*Flax Reader used to be called AO3 Reader. Updating keeps your library, progress and settings.*

## ⚡ Light and fast

Speed and a small footprint aren't an afterthought: they're what Flax Reader is built around. It should feel instant on any PC or phone, old ones included, and leave the rest of your memory to everything else you run.

| | 🪟 Windows | 🤖 Android |
| :--- | :--- | :--- |
| **Download** | **6.3 MB** zip: one 15 MB exe, no installer, no runtime | **10.8 MB** APK |
| **Memory in the library** | **8.6 MB** | 25 MB |
| **Memory while reading** | **10.8 MB** (*Alice in Wonderland*), **13.1 MB** (*Moby-Dick*, 215,000 words) | 33 MB (a 120,000-word novel) |
| **Start-up** | **0.12 s** to a usable window (0.47 s the first time after a restart) | 0.7–0.8 s cold start |

<sub>Measured 31 October 2026 on version 2.5. Windows: Task Manager's Memory column (private working set) on an Intel Core i5-13400F, Windows 11. Android: total PSS from `dumpsys meminfo` and `am start -W` on the Android 15 emulator; real phones differ a little.</sub>

**How it stays light**
- **Native, with no web engine.** Windows draws with Win32, Direct2D (on the CPU, so it needs no GPU driver) and DirectWrite. Android uses its own views and text layout, with no AndroidX, Compose or WebView. No Electron, no bundled browser.
- **One Rust core** does the heavy lifting for both apps: reading EPUBs and PDFs, the library, search, the sites and the PDF engine.
- **Nothing waits on the screen.** Downloads, imports, searches and PDF pages happen on background threads, so scrolling and page turns stay smooth.
- **Only what's needed, when it's needed.** Books are laid out a chapter at a time, PDF pages are rendered as they come into view (in parallel tiles on Windows) and kept within a fixed memory budget, and word counts for progress are worked out once per book.
- **Nothing in the background.** No tracking, no analytics, no services. Followed feeds are checked at most every 30 minutes, one at a time.

## ✨ At a glance

<table>
<tr>
<td width="33%" valign="top">

### 📚 Your books
EPUBs with their pictures, headings and tables, and PDFs page by page, sorted into shelves and your own collections.

</td>
<td width="33%" valign="top">

### 🌐 Free books and stories
Project Gutenberg and Standard Ebooks built in, SpaceBattles and Sufficient Velocity stories saved by their threadmarks, and any OPDS catalogue you add.

</td>
<td width="33%" valign="top">

### 🎀 Archive of Our Own
Search with AO3's own filters, follow authors and tags, save works to read offline, read and post comments, and bring in your bookmarks.

</td>
</tr>
<tr>
<td valign="top">

### 🖍️ Highlights and notes
Four highlight colours, a note on any highlight, word lookups from Wiktionary, and copy for any passage.

</td>
<td valign="top">

### 🔔 Following
Stories update themselves as chapters come out. Shelves and AO3 feeds show what's new, with an unread badge.

</td>
<td valign="top">

### ♿ Made for everyone
Dyslexia-friendly fonts, a reading ruler, bold word starts, read aloud, your own colours, and screen reader support.

</td>
</tr>
<tr>
<td valign="top">

### 🪶 Light as a feather
About 10 MB of memory while you read, a 6 MB download, and a window that opens in about a tenth of a second. No web engine.

</td>
<td valign="top">

### 🎨 Nine themes
Linen, Paper Light, Warm Sepia, Dark Slate, OLED Black, Nord Arctic, Midnight Blue, High Contrast and your own colours.

</td>
<td valign="top">

### 🔒 Yours, offline
Books stay on your device and open without internet. No ads, no tracking, no account.

</td>
</tr>
</table>

## 📸 A closer look

<table>
<tr>
<td width="50%"><img src="docs/screenshots/windows/library.png" alt="The library with seven classics, one marked as a favourite"></td>
<td width="50%"><img src="docs/screenshots/windows/pictures.png" alt="Alice's Adventures in Wonderland opening on its illustrated cover"></td>
</tr>
<tr>
<td align="center"><b>Library</b>: shelves, favourites, progress and search</td>
<td align="center"><b>Pictures</b>: covers and illustrations, as the book has them</td>
</tr>
<tr>
<td><img src="docs/screenshots/windows/define.png" alt="The definition of curiosity from Wiktionary, beside the book"></td>
<td><img src="docs/screenshots/windows/browse.png" alt="Project Gutenberg's most popular books in Browse"></td>
</tr>
<tr>
<td align="center"><b>Define</b>: meanings from Wiktionary, beside the page</td>
<td align="center"><b>Browse</b>: Project Gutenberg, Standard Ebooks, AO3 and more</td>
</tr>
<tr>
<td><img src="docs/screenshots/windows/themes.png" alt="Nine colour themes in Settings"></td>
<td><img src="docs/screenshots/windows/accessibility.png" alt="Accessibility settings: reading ruler, bold word starts, focus mode and easier fonts"></td>
</tr>
<tr>
<td align="center"><b>Themes</b>: nine of them, or your own colours</td>
<td align="center"><b>Accessibility</b>: reading aids, easier text and colour</td>
</tr>
</table>

### 📱 On Android

<p align="center">
<img src="docs/screenshots/android/library.png" width="19%" alt="Library on Android">
<img src="docs/screenshots/android/ao3.png" width="19%" alt="Works with AO3 details: rating, category, relationship and summary">
<img src="docs/screenshots/android/reader.png" width="19%" alt="Reading on Android with the menu bars shown">
<img src="docs/screenshots/android/browse.png" width="19%" alt="Project Gutenberg on Android">
<img src="docs/screenshots/android/settings.png" width="19%" alt="Themes on Android">
</p>

The same reader, highlights, dictionary, catalogues, Following and themes, built on the same Rust core. Big, thumb-friendly reader bars; tap the sides to turn pages.

---

## 🧭 Features

### 📖 Reading
- **EPUB and PDF:** EPUB books with their pictures, headings, lists and tables; PDFs page by page, as they were made, with zoom, the notes and comments left in them, password-protected PDFs, and true black pages with the OLED Black theme.
- **Pages or scroll:** turn pages like a book, with two pages side by side on wide windows, or scroll through each chapter.
- **Typography:** text size, line height, paragraph spacing, justified text and indents. Use any installed font or a font file on Windows; sans, serif or mono on Android.
- **Getting around:** contents, bookmarks with notes, and search across the whole book.
- **Progress:** chapter, page and time left. Your reading speed is learned from how fast you turn pages, or you can set it yourself.
- **Read aloud:** with your system's own voices, turning pages as it goes.

### 🖍️ Highlights, notes and the dictionary
- **Highlight:** select text (drag, double-click a word, or press and hold on Android) and pick one of four colours.
- **Notes:** add a note to any highlight. Every highlight and note is listed in Contents, and takes you back to its place.
- **Look words up:** select a word and choose Define for its meanings from Wiktionary.
- **Copy:** copy any passage.

### 🎀 Archive of Our Own
*Not yet tested against the live site; see the note at the top.*
- **Search** with AO3's own filters: title, author, fandoms, relationships, characters, tags, rating, category, completion, crossovers, word count and language, sorted the way you like.
- **Paste a link** to any work to save it to your library, or open an AO3 page you saved.
- **The work's details** come first, as on the Archive: rating, warnings, category, fandoms, relationships, characters, tags, summary and the author's notes.
- **Follow** authors, tags and collections, and **leave kudos** from the reader. Sign in to read works that are only for logged-in users.
- **Comments:** read a chapter's comments in Contents › Comments, and post your own when you're signed in.
- **Your AO3 lists:** bring in your bookmarks (as favourites, in a collection of their own) and Marked for Later (on your For later shelf). They download when you open them.
- **AO3's own EPUB:** save the EPUB AO3 makes of any work, from its ⋯ menu.
- **Your AO3 downloads:** import EPUBs you downloaded from AO3, and their tags, warnings and summaries come with them. *(Works now.)*

### 👤 Accounts
- **One place to sign in:** Archive of Our Own, SpaceBattles and Sufficient Velocity, each with its own card under Accounts (Settings › Sources on Android).
- **Bring in what you keep there:** AO3 bookmarks and Marked for Later, and the threads you watch on the forums, each as a collection in your library.
- **Your passwords go only to that site** and are never saved: Flax Reader keeps the site's own sign-in cookie, which signing out removes. On Android it's left out of cloud backups.
- **Not here:** Royal Road asks for a browser check, which Flax Reader doesn't do. Catalogues with a sign-in (Standard Ebooks for patrons, Calibre, Kavita, Komga) keep it with the catalogue.

### 🌐 Finding books
- **Your own files:** open EPUB books, PDFs and AO3 web pages with **Import your own files** in Browse, or open them with Flax Reader from your file manager.
- **Project Gutenberg:** browse and search tens of thousands of free books, built in.
- **Standard Ebooks:** carefully edited public-domain books, built in. Its newest releases are free to everyone; its patrons can add the full catalogue with their sign-in.
- **SpaceBattles and Sufficient Velocity:** browse the story boards, and save any story's chapters (its "threadmarks") to your library.
- **Your catalogues:** add any OPDS catalogue, such as Calibre's content server, Kavita, Komga or a library's catalogue, with a sign-in if it needs one.
- **Other sites:** Flax Reader only fetches from sites that answer a plain request. Sites behind a bot check (FanFiction.net, Scribble Hub, Webnovel, Feedbooks) aren't fetched; save their pages or books and import them instead.

### 🔔 Following
- **Stories:** paste a link to a story on SpaceBattles or Sufficient Velocity and it's saved to your library and kept up to date as new chapters come out. Royal Road stories tell you about new chapters too, which open in your browser (Royal Road's chapter pages ask for a browser check).
- **Shelves:** follow any catalogue shelf, such as Project Gutenberg's latest books, Standard Ebooks' new releases or a forum's newest stories, to see what's added.
- **AO3:** follow authors, tags and collections.
- **New and updated** works and chapters are marked, and a badge on Following shows how many are unread. Feeds are checked at most every 30 minutes, one at a time.

### 📚 Library
- **Shelves:** Reading, Completed, For later and Favourites.
- **Collections:** make your own, like folders of books, and put a book in as many as you like. Pick one to see just its books.
- **Find and sort:** search your library by title, author or tag, and sort by recently read, title, date added or author.
- **Export:** save a book as a web page, Markdown or plain text.

### ♿ Accessibility
- **Easier text:** Atkinson Hyperlegible and OpenDyslexic fonts are built in, with extra space between letters and words.
- **Reading aids:** a reading ruler, bold word starts and focus mode.
- **Colour:** a High Contrast theme, page tints and your own colours, each checked for readable contrast.
- **Interface:** a larger interface size, larger touch targets on Android, and keyboard and screen reader support on Windows (Narrator and NVDA) and Android (TalkBack).

### 🎨 Themes and settings
- **Nine themes:** Linen, Paper Light, Warm Sepia, Dark Slate, OLED Black, Nord Arctic, Midnight Blue, High Contrast and your own colours. They can follow your system's light or dark mode.
- **Settings in categories:** Appearance, Text, Reading, Accessibility, Library & files, Sources and About (and Updates and Shortcuts on Windows).
- **Updates:** the Windows app can check GitHub for new versions.

---

## 📥 Install

<table>
<tr>
<td width="50%" valign="top">

**🪟 Windows**
1. Download `flax-reader-…-windows-x64.zip` from [Releases](https://github.com/AgastyaShenoy/flax-reader/releases/latest).
2. Extract it and run `FlaxReader.exe`. There's no installer.

Requires 64-bit Windows 10 or 11. The exe isn't code-signed, so SmartScreen may ask you to confirm the first time.

</td>
<td width="50%" valign="top">

**🤖 Android**
1. Download `flax-reader-…-android.apk` from [Releases](https://github.com/AgastyaShenoy/flax-reader/releases/latest) on your phone.
2. Open it. If Android asks, allow your browser or file manager to install apps. It installs over AO3 Reader and keeps your library.

Requires Android 8.0 or newer on a 64-bit phone or tablet. The app isn't on the Play Store, so Play Protect may ask you to confirm.

</td>
</tr>
</table>

---

## ⌨️ Keyboard shortcuts (Windows)

| Keys | Action |
| :--- | :--- |
| `→` `PgDn` `Space` | Next page |
| `←` `PgUp` `Shift+Space` | Previous page |
| `Home` / `End` | First / last page of the chapter |
| `Ctrl+→` / `Ctrl+←` | Next / previous chapter |
| `Ctrl+F` | Search in the book |
| `Ctrl+B` | Bookmark this page |
| Double-click | Select a word, to highlight, copy or define it |
| `Ctrl+C` | Copy the selected text |
| `Ctrl+=` / `Ctrl+-` | Larger / smaller text |
| `R` | Read aloud, or stop |
| `F` | Focus mode |
| `F11` | Full screen |
| `Esc` | Clear a selection, close a panel, or leave focus mode or full screen |

---

## 💾 Where your data is kept

Everything is plain JSON in one folder: `%APPDATA%\ao3reader\ao3reader\data` on Windows (the folder kept its old name, so updates keep your library), and the app's private storage on Android. To use another folder (for a portable copy, say), set the `AO3READER_DATA_DIR` environment variable.

| File | What it holds |
| :--- | :--- |
| `library.json` | Your library: each book's details, shelf, favourite, progress, bookmarks, highlights and notes |
| `works/<id>.json` | The text of each book, with its pictures (or the PDF itself) beside it |
| `follows.json` | Followed stories, shelves and AO3 authors, tags and collections, and what's new in them |
| `config.json` | Settings, theme, your catalogues, your AO3 sign-in and your learned reading speed |

Saves are crash-safe. Each file is written to a temporary file first and then swapped in, so it's never left half-written. A `.bak` copy is kept from when the app last started. If a file can't be read, it's set aside and the last good copy is used.

---

## 🛠️ How it's built

- **Shared core (Rust):** EPUB and PDF import, OPDS catalogues, the Wiktionary lookup, the AO3 client, library storage and theme colours. Both apps use it. PDFs are drawn by a pure-Rust engine in `crates/pdf-engine` and `crates/pdf-render`.
- **Windows:** plain Win32 with Direct2D rendering on the CPU and DirectWrite text. Text boxes are native edit controls.
- **Android:** Kotlin with the platform's own views and no AndroidX. It talks to the Rust core through one JNI call that passes JSON. See [android/README.md](android/README.md).

### Build from source

Requires [Rust](https://www.rust-lang.org/); `rust-toolchain.toml` picks the right version (1.99) for you.

```bash
git clone https://github.com/AgastyaShenoy/flax-reader.git
cd flax-reader
cargo build --release --features native-ui --bin ao3reader-native
```

The Windows app is `target/release/ao3reader-native.exe` (released as `FlaxReader.exe`). For Android, see [android/README.md](android/README.md).

---

## 💬 Community

Come and chat on [Discord](https://discord.gg/3xuJY8UXKu). It's a general server, for talking about anything, and you can ask about Flax Reader there too. Found a bug? [Open an issue](https://github.com/AgastyaShenoy/flax-reader/issues).

## 🙏 Attribution

> **Ideated, prompted and guided by:** [Agastya Shenoy](https://github.com/AgastyaShenoy) (aga2522)
> **v1 developed majorly by AI:** Google **Gemini 3.8 Flash** via Antigravity
> **v2 (native Windows rewrite, Android app) and Flax Reader:** developed with **Claude** (Anthropic) in Claude Code

Sample books in the screenshots are public-domain novels from Project Gutenberg; the AO3-style details on the Android screenshot are made-up samples for the demo. Definitions come from Wiktionary (CC BY-SA). Atkinson Hyperlegible is by the Braille Institute and OpenDyslexic by Abbie Gonzalez, both under the SIL Open Font License.

Flax Reader is unofficial and isn't affiliated with Archive of Our Own or the Organization for Transformative Works.

## 📄 License

Copyright (C) 2026 Agastya Shenoy.

Flax Reader is free software: you can redistribute it and/or modify it under the terms of the [GNU General Public License](LICENSE) as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. It is distributed in the hope that it will be useful, but without any warranty; see the License for details.

In short: use it, share it and change it freely, and anyone who shares a changed version must share its source under the same terms. Versions up to 2.5.0 were released under the MIT licence and stay under it. The PDF rasteriser in `crates/pdf-engine/src/raster` keeps its own Apache-2.0 / MIT licence, alongside its code.
