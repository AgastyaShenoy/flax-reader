package org.ao3reader

import android.content.Intent
import android.text.Editable
import android.text.TextWatcher
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.BaseAdapter
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.PopupMenu
import android.widget.ProgressBar
import org.json.JSONObject

private val SHELVES = listOf("reading" to "Reading", "completed" to "Completed", "later" to "For later", "favorites" to "Favorites")
private val SORTS = listOf("Recently read", "Title", "Recently added", "Author")
private val EXPORTS = listOf(Triple("html", "Web page", "text/html"), Triple("md", "Markdown", "text/markdown"), Triple("txt", "Plain text", "text/plain"))

private fun inFilter(e: JSONObject, f: Int): Boolean = when (f) {
    0 -> true
    1 -> e.getString("shelf") == "reading"
    2 -> e.getString("shelf") == "completed"
    3 -> e.getString("shelf") == "later"
    else -> e.getBoolean("favorite") || e.getString("shelf") == "favorites"
}

private fun matches(e: JSONObject, q: String): Boolean {
    if (q.isBlank()) return true
    val m = e.getJSONObject("meta")
    val needle = q.trim().lowercase()
    val fields = listOf(m.getString("title")) + listOf("authors", "fandoms", "relationships", "characters", "tags").flatMap { m.getJSONArray(it).strings() }
    return fields.any { it.lowercase().contains(needle) }
}

fun libraryScreen(a: MainActivity): View {
    val p = Settings.pal
    val st = a.library
    val whole = Core.arr("library").objects()
    val collections = Core.arr("collections").objects()
    // A collection that's gone shows the whole library again.
    val current = collections.firstOrNull { it.getString("id") == st.collection }
    if (current == null) st.collection = null
    val members = current?.getJSONArray("works")?.strings()?.toSet()
    // The shelves count, and filter, within the collection shown.
    val all = if (members == null) whole else whole.filter { it.getJSONObject("meta").getString("id") in members }
    val box = a.column()
    box.addView(a.header("Library", logo = true))

    val search = a.input("Search title, author or tag")
    search.setText(st.query)
    box.addView(search, lp(top = 0).apply { leftMargin = a.dp(16); rightMargin = a.dp(16) })

    val chips = a.row().apply { setPadding(a.dp(16), a.dp(10), a.dp(16), a.dp(6)) }
    box.addView(HorizontalScrollView(a).apply { isHorizontalScrollBarEnabled = false; addView(chips) }, lp())
    if (whole.isNotEmpty()) box.addView(HorizontalScrollView(a).apply { isHorizontalScrollBarEnabled = false; addView(collectionsRow(a, collections, current, whole.size)) }, lp())

    val list = ListView(a).apply {
        divider = null
        dividerHeight = a.dp(10)
        setPadding(a.dp(16), a.dp(6), a.dp(16), a.dp(16))
        clipToPadding = false
        selector = rounded(0, 0f)
    }
    val empty = a.column().apply { gravity = Gravity.CENTER_HORIZONTAL; setPadding(a.dp(24), a.dp(40), a.dp(24), 0) }
    box.addView(empty, lp())
    box.addView(list, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))

    var shown = listOf<JSONObject>()
    val adapter = object : BaseAdapter() {
        override fun getCount() = shown.size
        override fun getItem(i: Int) = shown[i]
        override fun getItemId(i: Int) = i.toLong()
        override fun getView(i: Int, convert: View?, parent: ViewGroup?): View = workCard(a, shown[i], collections)
    }
    list.adapter = adapter

    fun refresh() {
        var l = all.filter { inFilter(it, st.filter) && matches(it, st.query) }
        l = when (st.sort) {
            1 -> l.sortedBy { it.getJSONObject("meta").getString("title").lowercase() }
            2 -> l.sortedByDescending { it.getLong("added") }
            3 -> l.sortedBy { it.getJSONObject("meta").getJSONArray("authors").optString(0).lowercase() }
            else -> l
        }
        shown = l
        adapter.notifyDataSetChanged()
        empty.removeAllViews()
        empty.gone(l.isNotEmpty())
        if (current != null && all.isEmpty()) {
            empty.addView(a.label("“${current.getString("name")}” is empty. Use ⋮ on a book to add it here.", 15f, p.muted).apply { gravity = Gravity.CENTER })
        } else if (whole.isEmpty()) {
            empty.addView(a.label("Your library is empty. Search AO3 or paste a link to a work to start reading.", 15f, p.muted).apply { gravity = Gravity.CENTER })
            val buttons = a.row().apply { gravity = Gravity.CENTER }
            buttons.addView(a.button("Search AO3", Kind.Primary, R.drawable.ic_search) { a.catalogs.switch(0); a.show(MainActivity.SEARCH) })
            buttons.addView(a.button("Import a file") { a.show(MainActivity.IMPORT) }, lp(LinearLayout.LayoutParams.WRAP_CONTENT).apply { leftMargin = a.dp(8) })
            empty.addView(buttons, lp(top = a.dp(16)))
        } else if (l.isEmpty()) {
            empty.addView(a.label("No works match this search or filter.", 15f, p.muted).apply { gravity = Gravity.CENTER })
        }

        chips.removeAllViews()
        val names = listOf("All", "Reading", "Completed", "For later", "Favorites")
        names.forEachIndexed { f, n ->
            val chip = a.countChip(n, all.count { inFilter(it, f) }, f == st.filter) { st.filter = f; st.listPos = 0; st.listTop = 0; refresh(); list.setSelection(0) }
            chips.addView(chip, lp(LinearLayout.LayoutParams.WRAP_CONTENT).apply { rightMargin = a.dp(8) })
        }
        val sort = a.label("⇅ ${SORTS[st.sort]}", 13.5f, p.text).apply {
            background = rounded(0, a.dpf(16), p.border, a.dp(1))
            setPadding(a.dp(14), a.dp(7), a.dp(14), a.dp(7))
        }
        sort.setOnClickListener {
            val m = a.popupMenu(sort)
            SORTS.forEachIndexed { i, s -> m.menu.add(0, i, i, s).apply { isCheckable = true; isChecked = i == st.sort } }
            m.setOnMenuItemClickListener { st.sort = it.itemId; refresh(); true }
            m.show()
        }
        chips.addView(sort)
    }

    // Stay where the list was: tapping a heart or a shelf on the 30th book rebuilds the screen.
    list.setOnScrollListener(object : android.widget.AbsListView.OnScrollListener {
        override fun onScrollStateChanged(v: android.widget.AbsListView?, state: Int) {}
        override fun onScroll(v: android.widget.AbsListView?, first: Int, visible: Int, total: Int) {
            if (total == 0) return
            st.listPos = first
            st.listTop = v?.getChildAt(0)?.top ?: 0
        }
    })

    search.addTextChangedListener(object : TextWatcher {
        override fun beforeTextChanged(s: CharSequence?, a1: Int, b: Int, c: Int) {}
        override fun onTextChanged(s: CharSequence?, a1: Int, b: Int, c: Int) {}
        override fun afterTextChanged(s: Editable?) {
            st.query = s?.toString() ?: ""
            refresh()
        }
    })
    refresh()
    list.setSelectionFromTop(st.listPos, st.listTop)
    return box
}

/** The collections row: a chip per collection (pick one to see only its books), one to make a new
 *  one, and ⋮ to rename or delete the one shown. */
private fun collectionsRow(a: MainActivity, collections: List<JSONObject>, shown: JSONObject?, total: Int): View {
    val p = Settings.pal
    val st = a.library
    val row = a.row().apply { setPadding(a.dp(16), a.dp(2), a.dp(16), a.dp(6)) }
    val gap = { LinearLayout.LayoutParams(-2, -2).apply { rightMargin = a.dp(8) } }
    row.addView(a.label("Collections", 13.5f, p.muted, bold = true), LinearLayout.LayoutParams(-2, -2).apply { rightMargin = a.dp(10) })
    if (collections.isNotEmpty()) row.addView(a.countChip("All books", total, shown == null) { st.collection = null; st.listPos = 0; st.listTop = 0; a.rebuild() }, gap())
    for (c in collections) {
        val id = c.getString("id")
        val on = id == st.collection
        row.addView(a.countChip(c.getString("name"), c.getJSONArray("works").length(), on) { st.collection = if (on) null else id; st.listPos = 0; st.listTop = 0; a.rebuild() }, gap())
    }
    row.addView(a.countChip("+ New collection", 0, false) {
        nameCollection(a, "New collection", "", "Create") { name ->
            val id = Core.req("collections.create", json("name" to name)) as? String
            if (id != null) st.collection = id
            a.rebuild()
        }
    }, gap())
    if (shown != null) {
        val name = shown.getString("name")
        lateinit var more: View
        more = a.iconButton(R.drawable.ic_more, p.text, "Rename or delete “$name”") {
            val m = a.popupMenu(more)
            m.menu.add(0, 0, 0, "Rename")
            m.menu.add(0, 1, 1, "Delete collection")
            m.setOnMenuItemClickListener {
                val id = shown.getString("id")
                if (it.itemId == 0) {
                    nameCollection(a, "Rename collection", name, "Rename") { new -> Core.req("collections.rename", json("id" to id, "name" to new)); a.rebuild() }
                } else {
                    a.dialog("Delete “$name”?", "The collection goes; its books stay in your library.", ok = "Delete", okKind = Kind.Danger) {
                        Core.req("collections.delete", json("id" to id))
                        st.collection = null
                        a.toast("Deleted the collection “$name”")
                        a.rebuild()
                        true
                    }
                }
                true
            }
            m.show()
        }
        row.addView(more)
    }
    return row
}

/** Asks for a collection's name, starting from `initial`; `done` gets it, trimmed and not blank. */
private fun nameCollection(a: MainActivity, title: String, initial: String, ok: String, done: (String) -> Unit) {
    val input = a.input("Name the collection").apply {
        setText(initial)
        setSelection(0, initial.length)
        inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
    }
    val d = a.dialog(title, body = input, ok = ok) {
        val name = input.text.toString().trim()
        if (name.isNotEmpty()) done(name)
        name.isNotEmpty()
    }
    input.requestFocus()
    d.window?.setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_STATE_VISIBLE or android.view.WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
}

private fun workCard(a: MainActivity, e: JSONObject, collections: List<JSONObject>): View {
    val p = Settings.pal
    val m = e.getJSONObject("meta")
    val id = m.getString("id")
    val card = a.card()

    val top = a.row()
    top.addView(a.label(m.getString("title"), 17f, p.text, bold = true, lines = 2).apply { setOnClickListener { a.openWork(id) } },
        LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
    val fav = e.getBoolean("favorite")
    top.addView(a.iconButton(if (fav) R.drawable.ic_heart_fill else R.drawable.ic_heart, if (fav) 0xFFE11D48.toInt() else p.muted, if (fav) "Remove from favourites" else "Add to favourites") {
        Core.req("library.favorite", json("id" to id))
        a.rebuild()
    })
    card.addView(top)

    val fandoms = m.getJSONArray("fandoms").strings().take(2)
    val authors = m.getJSONArray("authors").strings()
    // No "by" for a book that names no author (a PDF that doesn't say).
    val by = listOfNotNull(authors.takeIf { it.isNotEmpty() }?.let { "by " + it.joinToString(", ") }, fandoms.takeIf { it.isNotEmpty() }?.joinToString(", ")).joinToString("  ·  ")
    if (by.isNotEmpty()) card.addView(a.label(by, 13f, p.muted, lines = 1), lp(top = a.dp(2)))

    card.addView(a.pillScroller(workPills(a, m)), lp(top = a.dp(10)))

    val summary = m.getString("summary").split(Regex("\\s+")).joinToString(" ").trim()
    if (summary.isNotEmpty()) card.addView(a.label(summary, 13.5f, alpha(p.text, 0.85f), lines = 3).apply { setLineSpacing(0f, 1.15f) }, lp(top = a.dp(10)))

    val started = e.getInt("chapter") > 1 || e.getDouble("percentage") > 0 || e.getDouble("position") > 0
    val completed = e.getBoolean("completed")
    val pct = e.getDouble("percentage")
    // A PDF counts pages instead of chapters and words (none yet if it's locked and not opened).
    val isPdf = !m.isNull("pdf_pages")
    val pdfPages = m.optInt("pdf_pages", 0)
    val status = when {
        completed -> "Finished"
        isPdf && pdfPages == 0 -> "Locked with a password"
        pdfPages > 0 && started -> "Page ${minOf(e.getDouble("position").toInt() + 1, pdfPages)} of $pdfPages  ·  ${pct.toInt()}%"
        pdfPages > 0 -> "$pdfPages pages  ·  not started"
        started -> "Chapter ${e.getInt("chapter")} of ${m.getInt("chapters")}  ·  ${pct.toInt()}%"
        else -> "${m.getInt("chapters")} chapters  ·  not started"
    }
    val prog = a.row()
    prog.addView(a.label(status, 12.5f, alpha(p.text, 0.85f)))
    prog.addView(ProgressBar(a, null, android.R.attr.progressBarStyleHorizontal).apply {
        max = 1000
        progress = (pct * 10).toInt()
        progressTintList = android.content.res.ColorStateList.valueOf(p.accentMark)
        progressBackgroundTintList = android.content.res.ColorStateList.valueOf(p.border)
    }, LinearLayout.LayoutParams(0, a.dp(4), 1f).apply { leftMargin = a.dp(12) })
    card.addView(prog, lp(top = a.dp(12)))
    val stats = when {
        isPdf -> "PDF"
        // Brought in from a list on a site (AO3 bookmarks, watched threads): opening downloads it.
        !e.optBoolean("downloaded", true) -> "Not downloaded yet: opening it downloads it"
        else -> "${thousands(m.getLong("words"))} words" + if (m.getLong("kudos") > 0) "  ·  ${thousands(m.getLong("kudos"))} kudos" else ""
    }
    card.addView(a.label(stats, 12f, p.muted), lp(top = a.dp(4)))

    val actions = a.row()
    actions.addView(a.button(if (completed) "Read again" else if (started) "Resume" else "Read", Kind.Tonal, R.drawable.ic_book) { a.openWork(id) })
    val shelf = SHELVES.firstOrNull { it.first == e.getString("shelf") }?.second ?: "Reading"
    val shelfBtn = a.button(shelf) {}
    shelfBtn.setOnClickListener {
        val menu = a.popupMenu(shelfBtn)
        SHELVES.forEachIndexed { i, (key, name) -> menu.menu.add(0, i, i, name).apply { isCheckable = true; isChecked = key == e.getString("shelf") } }
        menu.setOnMenuItemClickListener {
            Core.req("library.shelf", json("id" to id, "shelf" to SHELVES[it.itemId].first))
            a.rebuild()
            true
        }
        menu.show()
    }
    actions.addView(shelfBtn, lp(LinearLayout.LayoutParams.WRAP_CONTENT).apply { leftMargin = a.dp(8) })
    actions.addView(View(a), LinearLayout.LayoutParams(0, 1, 1f))
    lateinit var more: View
    more = a.iconButton(R.drawable.ic_more, p.text, if (pdfPages > 0) "More: collections or delete" else "More: collections, export or delete") {
        val menu = a.popupMenu(more)
        // In or out of each collection (ids from 100), a new one, export (a PDF is already its
        // own file: nothing to export), and delete.
        collections.forEachIndexed { i, c ->
            val has = c.getJSONArray("works").strings().contains(id)
            menu.menu.add(0, 100 + i, i, if (has) "Remove from “${c.getString("name")}”" else "Add to “${c.getString("name")}”")
        }
        menu.menu.add(0, 98, 50, "New collection…")
        if (pdfPages == 0) EXPORTS.forEachIndexed { i, (_, name, _) -> menu.menu.add(0, i, 60 + i, "Export as $name") }
        // AO3 makes its own EPUB of each work (with its tags and notes), to keep or read elsewhere.
        if (m.optBoolean("ao3", false)) menu.menu.add(0, 97, 70, "Save AO3's EPUB…")
        menu.menu.add(0, 99, 99, "Delete from library")
        menu.setOnMenuItemClickListener {
            when {
                it.itemId >= 100 -> {
                    val c = collections[it.itemId - 100]
                    val on = !c.getJSONArray("works").strings().contains(id)
                    Core.req("collections.set", json("id" to c.getString("id"), "work" to id, "on" to on))
                    a.toast(if (on) "Added to “${c.getString("name")}”" else "Removed from “${c.getString("name")}”")
                    a.rebuild()
                }
                it.itemId == 98 -> nameCollection(a, "New collection", "", "Create") { name ->
                    Core.req("collections.create", json("name" to name, "work" to id))
                    a.toast("Added to “$name”")
                    a.rebuild()
                }
                it.itemId == 99 -> confirmDelete(a, id, m.getString("title"))
                it.itemId == 97 -> {
                    a.toast("Getting AO3's EPUB…")
                    downloadEpub(a, id, m.getString("title")) { err -> if (err != null) a.toast(err) }
                }
                else -> export(a, id, it.itemId)
            }
            true
        }
        menu.show()
    }
    actions.addView(more)
    card.addView(actions, lp(top = a.dp(12)))
    return card
}

/** Rating, categories, warnings (other than "none"), up to `ships` relationships and the
 *  complete/in-progress status: the pills on a work card. `w` is a work's meta JSON. */
fun workPills(a: MainActivity, w: JSONObject, ships: Int = 3): LinearLayout {
    val pills = a.row()
    val gap = { lp(LinearLayout.LayoutParams.WRAP_CONTENT).apply { rightMargin = a.dp(6) } }
    // Books from files have no AO3 rating or completion status: only their real tags show.
    val ao3 = w.optBoolean("ao3", true)
    if (ao3) pills.addView(a.markerPill(w.getString("rating"), Pal.opaque(w.getInt("rating_color"))), gap())
    for (c in w.getJSONArray("categories").objects()) pills.addView(a.markerPill(c.getString("name"), Pal.opaque(c.getInt("color"))), gap())
    for (x in w.getJSONArray("warnings").strings().filter { !it.contains("No Archive Warnings") }) pills.addView(a.markerPill(x, WARNING_RED), gap())
    for (r in w.getJSONArray("relationships").strings().take(ships)) pills.addView(a.shipPill(r), gap())
    if (ao3) pills.addView(a.tagPill(if (w.getBoolean("complete")) "Complete" else "In progress", quiet = true))
    return pills
}

const val WARNING_RED = 0xFFCC3838.toInt()

private fun confirmDelete(a: MainActivity, id: String, title: String) {
    a.dialog("Delete this work?", "“$title” and its downloaded text will be removed from your library.", ok = "Delete", okKind = Kind.Danger) {
        Core.req("library.delete", json("id" to id))
        a.toast("Removed “$title”")
        a.rebuild()
        true
    }
}

/** Exports through the system "save as" picker. */
private fun export(a: MainActivity, id: String, format: Int) {
    val (ext, _, mime) = EXPORTS[format]
    val res = try {
        Core.obj("export", json("id" to id, "format" to ext))
    } catch (e: CoreError) {
        a.toast(e.message ?: "Export failed")
        return
    }
    val name = res.getString("title").replace(Regex("[\\\\/:*?\"<>|]"), "_").take(100) + ".$ext"
    a.onDocument = { _, data ->
        val uri = data?.data
        if (uri != null) {
            runCatching { a.contentResolver.openOutputStream(uri)?.use { it.write(res.getString("text").toByteArray()) } }
                .onSuccess { a.toast("Saved $name") }.onFailure { a.toast("Couldn't save: ${it.message}") }
        }
    }
    @Suppress("DEPRECATION")
    a.startActivityForResult(Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
        addCategory(Intent.CATEGORY_OPENABLE)
        type = mime
        putExtra(Intent.EXTRA_TITLE, name)
    }, 1)
}
