package org.ao3reader

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import android.window.OnBackInvokedDispatcher

/** One activity; screens are plain views swapped in and out. */
class MainActivity : Activity() {
    private lateinit var root: FrameLayout
    private lateinit var shell: LinearLayout
    private lateinit var content: FrameLayout
    private lateinit var nav: LinearLayout
    private var tab = LIBRARY
    /** The Settings category open, or -1 for the list of them. */
    var settingsPage = -1
    /** The id of the book open in the reader. */
    private var openId: String? = null
    /** The look the open book was last drawn in (see `rebuild`). */
    private var readerLook = ""
    private var animateTab = false
    private var reader: BookView? = null

    val library = LibraryState()
    val search = SearchState()
    val catalogs = CatalogState()
    val following = FollowingState()
    val imports = ImportState()

    companion object {
        const val LIBRARY = 0
        const val SEARCH = 1
        const val FOLLOWING = 2
        const val SETTINGS = 3
        /** Opened from Settings; not in the tab bar. */
        const val ACCOUNT = 4
        const val IMPORT = 5
    }

    /** Called with the result of a document picker or creator started by a screen. */
    var onDocument: ((Int, Intent?) -> Unit)? = null

    override fun attachBaseContext(base: Context) {
        super.attachBaseContext(base)
        // Interface size (Settings → Accessibility): every dp and sp in the app scales with it.
        val scale = base.getSharedPreferences("ui", MODE_PRIVATE).getFloat("scale", 1f)
        if (scale != 1f) {
            applyOverrideConfiguration(Configuration().apply { densityDpi = (base.resources.configuration.densityDpi * scale).toInt() })
        }
    }

    /** Sets the interface size and starts the screen again at it (back on Settings). */
    fun setUiScale(scale: Float) {
        getSharedPreferences("ui", MODE_PRIVATE).edit().putFloat("scale", scale).apply()
        Settings.config.put("ui_scale", scale.toDouble())
        Settings.save()
        recreate()
    }

    override fun onSaveInstanceState(out: Bundle) {
        super.onSaveInstanceState(out)
        out.putInt("tab", if (tab == ACCOUNT || tab == IMPORT) SETTINGS else tab)
        out.putInt("settings_page", settingsPage)
        // The book being read, to reopen when Android recreates the screen (a change of font
        // size or display size, say).
        reader?.saveProgress()
        openId?.let { out.putString("open", it) }
    }

    override fun onDestroy() {
        // Stop reading aloud and drawing PDF pages: nothing will show them any more.
        reader?.saveProgress()
        reader?.close()
        super.onDestroy()
    }

    private fun isNight(c: Configuration) = (c.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES

    /** Dark mode switched on or off: "Match system" follows it. */
    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        val night = isNight(newConfig)
        if (night != Settings.night) {
            Settings.night = night
            if (Settings.config.optBoolean("follow_system")) rebuild()
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val init = Core.obj("init", json("dir" to filesDir.absolutePath))
        Settings.themes = init.getJSONArray("themes").objects().map(::Pal)
        Settings.load()
        Settings.night = isNight(resources.configuration)
        tab = savedInstanceState?.getInt("tab", LIBRARY) ?: LIBRARY
        settingsPage = savedInstanceState?.getInt("settings_page", -1) ?: -1

        root = FrameLayout(this)
        // Keep the first text field from taking focus (and the keyboard) on launch.
        root.isFocusableInTouchMode = true
        // When a text field gives up focus it lands here; don't grey the whole screen to show it.
        if (Build.VERSION.SDK_INT >= 26) root.defaultFocusHighlightEnabled = false
        setContentView(root)
        // Draw behind the status and navigation bars on every Android version (15 and later do it
        // anyway); the screens leave room for the bars themselves (applyInsets). Otherwise Android
        // 11–14 shrink the window whenever the reader shows its bars, and the page reflows.
        if (Build.VERSION.SDK_INT >= 30) window.setDecorFitsSystemWindows(false)
        if (Build.VERSION.SDK_INT >= 28) {
            window.attributes = window.attributes.apply { layoutInDisplayCutoutMode = android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES }
        }
        root.setOnApplyWindowInsetsListener { v, insets -> applyInsets(insets); insets }
        rebuild()

        if (Build.VERSION.SDK_INT >= 33) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT) { handleBack() }
        }
        val reopen = savedInstanceState?.getString("open")
        if (reopen != null) openWork(reopen)
        // "Open with" is handled once, not again when the screen is recreated.
        else if (savedInstanceState == null) openIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        openIntent(intent)
    }

    /** A book opened with the app from elsewhere ("Open with"): imported, then opened. */
    private fun openIntent(i: Intent?) {
        val uri = i?.takeIf { it.action == Intent.ACTION_VIEW }?.data ?: return
        i.data = null // handled once, even if the activity is shown again
        val tmp = java.io.File(cacheDir, "open.bin")
        val name = displayName(uri)
        toast("Opening the book…")
        // Copied off the main thread: a file from a cloud drive can be slow.
        Thread {
            val copied = runCatching { contentResolver.openInputStream(uri)?.use { input -> tmp.outputStream().use { input.copyTo(it) } } != null }.getOrDefault(false)
            runOnUiThread {
                if (!copied) return@runOnUiThread toast("Couldn't open that file")
                Core.background("import", json("path" to tmp.absolutePath, "name" to name)) { r ->
                    tmp.delete()
                    r.onSuccess { openWork((it as org.json.JSONObject).getString("id")) }.onFailure { toast(it.message ?: "Couldn't open that book") }
                }
            }
        }.start()
    }

    /** A document's name without its extension, if the app that shared it tells. */
    fun displayName(uri: android.net.Uri): String? = runCatching {
        contentResolver.query(uri, arrayOf(android.provider.OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0)?.substringBeforeLast('.') else null
        }
    }.getOrNull()

    @Deprecated("Used before Android 13")
    override fun onBackPressed() {
        handleBack()
    }

    private fun handleBack() {
        val r = reader
        when {
            r != null -> if (!r.back()) closeReader()
            tab == ACCOUNT || tab == IMPORT -> show(SETTINGS)
            tab == SETTINGS && settingsPage >= 0 -> openSettings(-1)
            // Within Browse: close the filters, leave a pasted link, step back up the shelves.
            tab == SEARCH && catalogs.source == 0 && search.filtersOpen -> { search.filtersOpen = false; rebuild() }
            tab == SEARCH && catalogs.source == 0 && search.link != null -> { search.link = null; rebuild() }
            tab == SEARCH && catalogs.source > 0 && catalogs.stack.size > 1 -> {
                catalogs.stack.removeAt(catalogs.stack.size - 1)
                catalogs.error = null
                catalogs.scroll = 0
                rebuild()
            }
            // Within Following: back from one followed feed to them all.
            tab == FOLLOWING && following.selected != null -> { following.selected = null; rebuild() }
            tab != LIBRARY -> show(LIBRARY)
            else -> finish()
        }
    }

    override fun onPause() {
        super.onPause()
        reader?.saveProgress()
    }

    @Deprecated("Document picker results")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (resultCode == RESULT_OK) onDocument?.invoke(requestCode, data)
        onDocument = null
    }

    private var insetTop = 0
    private var insetBottom = 0
    private var cutoutTop = 0
    private var hintShown = false

    @Suppress("DEPRECATION")
    private fun applyInsets(insets: WindowInsets) {
        val (top, bottom) = if (Build.VERSION.SDK_INT >= 30) {
            val i = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.ime())
            i.top to i.bottom
        } else insets.systemWindowInsetTop to insets.systemWindowInsetBottom
        insetTop = top
        insetBottom = bottom
        // Camera cutouts stay in place when the status bar hides; the page must avoid them.
        cutoutTop = when {
            Build.VERSION.SDK_INT >= 30 -> insets.getInsets(WindowInsets.Type.displayCutout()).top
            Build.VERSION.SDK_INT >= 28 -> insets.displayCutout?.safeInsetTop ?: 0
            else -> 0
        }
        shell.setPadding(0, top, 0, bottom)
        reader?.setInsets(top, bottom, cutoutTop)
    }

    /** Rebuilds every view, e.g. after the theme changes. */
    fun rebuild() {
        val p = Settings.pal
        root.removeAllViews()
        root.setBackgroundColor(p.bg)
        window.decorView.setBackgroundColor(p.bg)
        styleSystemBars(p)

        shell = column()
        content = FrameLayout(this)
        shell.addView(content, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        nav = row().apply {
            setBackgroundColor(p.surface)
            setPadding(0, dp(6), 0, dp(6))
        }
        shell.addView(View(this).apply { setBackgroundColor(p.border) }, lp(h = dp(1)))
        shell.addView(nav, lp())
        root.addView(shell, frame())
        shell.setPadding(0, insetTop, 0, insetBottom)

        buildNav()
        val screenView = screen(tab)
        content.addView(screenView, frame())
        if (animateTab) Anim.enter(screenView, dy = dpf(10), ms = 170)
        animateTab = false

        reader?.let { r ->
            // Only a change of look (theme, text settings) redraws the book: a background job
            // finishing must not close its open panel or drop the selection.
            val look = "${p.bg}|${Settings.config}|${Settings.typo}"
            if (look != readerLook) {
                readerLook = look
                r.rebuild()
            }
            root.addView(r.view, frame())
        }
        root.requestApplyInsets()
    }

    /** Redraws only the tab bar (e.g. a new unread count), leaving the screen as it is. */
    fun refreshNav() {
        if (::nav.isInitialized) buildNav()
    }

    private fun buildNav() {
        val p = Settings.pal
        nav.removeAllViews()
        val tabs = listOf(R.drawable.ic_book to "Library", R.drawable.ic_search to "Browse", R.drawable.ic_follow to "Following", R.drawable.ic_tune to "Settings")
        val unread = runCatching { Core.obj("follows").getInt("unread") }.getOrDefault(0)
        tabs.forEachIndexed { i, (res, name) ->
            val sel = i == tab || ((tab == ACCOUNT || tab == IMPORT) && i == SETTINGS)
            val item = column().apply {
                gravity = Gravity.CENTER
                setPadding(0, dp(4), 0, dp(4))
                // Settings again, from one of its categories: back to the list of them.
                setOnClickListener { if (i == SETTINGS && tab == SETTINGS) openSettings(-1) else show(i) }
            }
            val iconBox = FrameLayout(this)
            // Material-style indicator: a neutral pill behind the current tab's icon, which takes
            // the accent. Everything else in the bar stays quiet.
            if (sel) iconBox.background = rounded(p.selected, dpf(14))
            iconBox.addView(icon(res, if (sel) p.accentMark else p.muted, 24), frame(dp(24), dp(24), Gravity.CENTER))
            if (i == FOLLOWING && unread > 0) {
                // New works from followed authors and tags.
                iconBox.addView(label(if (unread > 99) "99+" else "$unread", 9.5f, p.onAccent, bold = true).apply {
                    gravity = Gravity.CENTER
                    background = rounded(p.accent, dpf(8))
                    setPadding(dp(4), 0, dp(4), 0)
                    minWidth = dp(16)
                }, frame(ViewGroup.LayoutParams.WRAP_CONTENT, dp(16), Gravity.TOP or Gravity.END))
                iconBox.contentDescription = "$unread new"
            }
            item.addView(iconBox, LinearLayout.LayoutParams(dp(56), dp(30)))
            item.addView(label(name, 11.5f, if (sel) p.text else p.muted, bold = sel, lines = 1).apply { gravity = Gravity.CENTER }, lp(top = dp(2)))
            nav.addView(item, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        }
    }

    fun show(i: Int) {
        animateTab = i != tab
        tab = i
        rebuild()
    }

    val currentTab get() = tab

    /** Opens a Settings category (-1: the list of them). */
    fun openSettings(page: Int) {
        settingsPage = page
        animateTab = true
        tab = SETTINGS
        rebuild()
    }

    private fun screen(i: Int): View = when (i) {
        SEARCH -> browseScreen(this)
        FOLLOWING -> followingScreen(this)
        SETTINGS -> settingsScreen(this)
        ACCOUNT -> accountScreen(this)
        IMPORT -> importScreen(this)
        else -> libraryScreen(this)
    }

    @Suppress("DEPRECATION")
    fun styleSystemBars(p: Pal) {
        window.statusBarColor = Color.TRANSPARENT
        window.navigationBarColor = Color.TRANSPARENT
        if (Build.VERSION.SDK_INT >= 30) {
            val light = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS
            window.insetsController?.setSystemBarsAppearance(if (p.dark) 0 else light, light)
        } else {
            window.decorView.systemUiVisibility = View.SYSTEM_UI_FLAG_LAYOUT_STABLE or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or
                View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION or (if (p.dark) 0 else View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR)
        }
    }

    /** Hides or shows the status and navigation bars (immersive reading). */
    fun immersive(on: Boolean) {
        if (Build.VERSION.SDK_INT >= 30) {
            val c = window.insetsController ?: return
            if (on) {
                c.hide(WindowInsets.Type.systemBars())
                c.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            } else c.show(WindowInsets.Type.systemBars())
        } else {
            @Suppress("DEPRECATION")
            window.decorView.systemUiVisibility = if (on) {
                View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY or View.SYSTEM_UI_FLAG_FULLSCREEN or View.SYSTEM_UI_FLAG_HIDE_NAVIGATION or
                    View.SYSTEM_UI_FLAG_LAYOUT_STABLE or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
            } else View.SYSTEM_UI_FLAG_LAYOUT_STABLE or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
        }
    }

    fun openWork(id: String) {
        // The open book's place is saved first: opening another replaces it in the core.
        reader?.saveProgress()
        val opened = try {
            Core.obj("work.open", json("id" to id))
        } catch (e: CoreError) {
            val story = if (e.message == "missing") Core.req("story.url", json("id" to id)) as? String else null
            when {
                e.message == "missing" && id.all { it.isDigit() } -> {
                    toast("Downloading this work again…")
                    startFetch(id, open = true)
                }
                story != null -> {
                    // A watched thread brought in from a forum: download it now.
                    toast("Downloading this story…")
                    Core.background("story.get", json("id" to id), slow = true) { r ->
                        r.onSuccess { openWork(id) }.onFailure { toast(it.message ?: "Couldn't download the story") }
                    }
                }
                else -> toast(e.message ?: "Couldn't open the work")
            }
            return
        }
        currentFocus?.let { v ->
            v.clearFocus()
            getSystemService(android.view.inputmethod.InputMethodManager::class.java)?.hideSoftInputFromWindow(v.windowToken, 0)
        }
        runCatching { Core.req("follows.read", json("work" to id)) }
        // PDFs show their pages as they are; everything else is laid out as text.
        val isPdf = !opened.getJSONObject("meta").isNull("pdf_pages")
        val r: BookView = try {
            if (isPdf) PdfScreen(this, id, opened) else ReaderScreen(this, id, opened)
        } catch (e: Exception) {
            // A damaged book (a PDF the engine can't read, a work with no chapters) stays shut.
            toast("Couldn't open this book: ${e.message ?: "it may be damaged"}")
            return
        }
        readerLook = "${Settings.pal.bg}|${Settings.config}|${Settings.typo}"
        reader?.let { it.close(); root.removeView(it.view) }
        reader = r
        openId = id
        root.addView(r.view, frame())
        r.setInsets(insetTop, insetBottom, cutoutTop)
        Anim.enter(r.view, dy = dpf(28), ms = 220)
        if (!hintShown) {
            hintShown = true
            toast("Tap the middle, top or bottom of the page for the menu; tap the sides to turn pages.")
        }
    }

    fun closeReader() {
        val r = reader ?: return
        r.saveProgress()
        r.close()
        reader = null
        openId = null
        immersive(false)
        // Rebuild the screens underneath, then let the reader fade away on top of them.
        rebuild()
        root.addView(r.view)
        Anim.exit(r.view, dy = dpf(28), ms = 160) { root.removeView(r.view) }
    }

    /** Downloads a work; `done` gets the work id on success, `failed` the error (else it's a toast). */
    fun startFetch(input: String, open: Boolean, done: ((String) -> Unit)? = null, failed: ((String) -> Unit)? = null) {
        Core.background("fetch", json("input" to input)) { r ->
            r.onSuccess {
                val o = it as org.json.JSONObject
                toast("Saved “${o.getJSONObject("meta").optString("title")}”")
                done?.invoke(o.getString("id"))
                if (open) openWork(o.getString("id"))
            }.onFailure {
                val msg = it.message ?: "Couldn't download the work"
                if (failed != null) failed(msg) else toast(msg)
            }
        }
    }

    fun toast(msg: String) = Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()

    fun header(title: String, sub: String? = null, logo: Boolean = false): View {
        val p = Settings.pal
        val box = row().apply { setPadding(dp(20), dp(18), dp(20), dp(10)) }
        if (logo) {
            // The flax flower on the home screen, top left, as on the desktop app.
            box.addView(ImageView(this).apply {
                setImageResource(R.drawable.ic_flax)
                imageTintList = android.content.res.ColorStateList.valueOf(p.accentMark)
                importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
            }, LinearLayout.LayoutParams(dp(30), dp(30)).apply { rightMargin = dp(12) })
        }
        box.addView(label(title, 26f, p.text, bold = true))
        if (sub != null) box.addView(label(sub, 13f, p.muted).apply { setPadding(dp(10), dp(8), 0, 0) })
        return box
    }
}

/** Remembers screen state across rebuilds. */
class LibraryState {
    var query = ""
    var filter = 0
    var sort = 0
    /** The collection shown, if one is picked: the shelves then filter within it. */
    var collection: String? = null
    /** Where the list was scrolled to (first row and its offset), kept across rebuilds. */
    var listPos = 0
    var listTop = 0
}

class ImportState {
    var busy = false
    var error: String? = null
    /** The work imported last: its meta JSON. */
    var done: org.json.JSONObject? = null
}

fun textOrEmpty(v: TextView) = v.text?.toString() ?: ""
