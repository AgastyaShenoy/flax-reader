package org.ao3reader

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.os.Handler
import android.os.Looper
import android.view.GestureDetector
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.OverScroller
import android.widget.TextView
import org.json.JSONObject
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/** The book open over the screens: laid-out text ([ReaderScreen]) or a PDF's pages ([PdfScreen]). */
interface BookView {
    val view: View
    /** Back button: closes a panel first. Returns false when the book itself should close. */
    fun back(): Boolean
    fun saveProgress()
    fun setInsets(t: Int, b: Int, cutout: Int)
    /** Recreates the chrome (after a theme change) while keeping the reading position. */
    fun rebuild()
    /** The book is closing: let go of what it holds. */
    fun close()
}

/** A PDF: its pages as they are, one under another at the screen's width. The Rust engine draws
 *  them on background threads; tap the middle for the toolbar. */
class PdfScreen(private val a: MainActivity, val workId: String, opened: JSONObject) : FrameLayout(a), BookView {
    private val meta: JSONObject = opened.getJSONObject("meta")
    private val pages = PdfPages(a, this)
    private var top = LinearLayout(a)
    private var pageLabel = TextView(a)
    private var chrome = false
    private var insetTop = 0
    private var insetBottom = 0
    private val handler = Handler(Looper.getMainLooper())
    private val saveLater = Runnable { saveProgress() }
    private var dirty = false

    init {
        val pdf = Core.obj("pdf.open", json("id" to workId))
        val sizes = pdf.getJSONArray("sizes")
        pages.sizes = (0 until sizes.length()).map { val s = sizes.getJSONArray(it); s.getDouble(0).toFloat() to s.getDouble(1).toFloat() }
        pages.start(pdf.getInt("doc"))
        pages.goTo(opened.optDouble("position", 0.0).toFloat())
        rebuild()
    }

    override val view: View get() = this

    override fun setInsets(t: Int, b: Int, cutout: Int) {
        insetTop = t
        insetBottom = b
        pages.safeTop = cutout
        top.setPadding(a.dp(6), t + a.dp(6), a.dp(6), a.dp(6))
    }

    override fun rebuild() {
        val p = Settings.pal
        removeAllViews()
        setBackgroundColor(p.bg)
        addView(pages, frame())
        pages.recolor()
        top = a.row().apply { setBackgroundColor(p.surface) }
        top.addView(a.iconButton(R.drawable.ic_back, p.text, "Back to library", large = true) { a.closeReader() })
        top.addView(a.label(meta.optString("title"), 18f, p.text, bold = true, lines = 1).apply { gravity = Gravity.CENTER_VERTICAL },
            LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { leftMargin = a.dp(6) })
        pageLabel = a.label("", 15f, p.muted, lines = 1).apply { setPadding(a.dp(8), 0, a.dp(14), 0) }
        top.addView(pageLabel)
        addView(top, frame(h = ViewGroup.LayoutParams.WRAP_CONTENT, gravity = Gravity.TOP))
        setInsets(insetTop, insetBottom, pages.safeTop)
        top.gone(!chrome)
        a.immersive(!chrome)
        positionChanged()
    }

    fun toggleChrome() {
        chrome = !chrome
        if (chrome) Anim.enter(top.also { it.gone(false) }, dy = -a.dpf(20), ms = 170) else top.gone(true)
        a.immersive(!chrome)
    }

    /** Updates the page shown in the toolbar and schedules a progress save. */
    fun positionChanged() {
        val n = pages.sizes.size
        pageLabel.text = "Page ${min(pages.currentPage() + 1, max(n, 1))} of $n"
        pages.contentDescription = "${meta.optString("title")}, page ${pages.currentPage() + 1} of $n"
        dirty = true
        handler.removeCallbacks(saveLater)
        handler.postDelayed(saveLater, 3000)
    }

    override fun saveProgress() {
        if (!dirty) return
        dirty = false
        runCatching { Core.req("progress.save", json("chapter" to 0, "position" to pages.anchor().toDouble(), "completed" to pages.atEnd())) }
    }

    override fun back(): Boolean = false

    override fun close() {
        handler.removeCallbacks(saveLater)
        pages.stop()
        runCatching { Core.req("pdf.close") }
    }
}

/** The pages, scrolled with a finger or by TalkBack. Each page is rendered whole at the screen's
 *  width, by the engine's render threads (the same ones as on Windows); zoomed in, the part of each
 *  page on screen is rendered again, sharp, once scrolling stops. Pages on screen and the next one
 *  are never evicted, so a big screen can't make pages push each other out and render over and
 *  over. (Small tiles would cost more here: a page the engine can't record is interpreted again
 *  for every tile.) */
class PdfPages(context: Context, private val screen: PdfScreen) : View(context) {
    var sizes: List<Pair<Float, Float>> = emptyList()
    var safeTop = 0
    private var doc = 0
    private var scrollPos = 0f
    private var pending: Float? = null
    private val gap get() = context.dpf(10)
    private val scroller = OverScroller(context)
    private val paint = Paint(Paint.FILTER_BITMAP_FLAG)
    private val boxPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private var look = 0

    // Page tops and sizes (in whole pixels) at the current width and zoom.
    /** Pixels per point as shown. */
    private var scale = 1f
    /** Pixels per point that fit the widest page to the screen. */
    private var fit = 1f
    /** 1 fits the widest page to the screen; up to 4 (pinch, or double-tap for 2). */
    private var zoom = 1f
    /** How far a zoomed page is panned sideways. */
    private var panX = 0f
    /** A pinch under way: drawn scaled about (pinchX, pinchY) until the fingers lift. */
    private var pinch = 1f
    private var pinchX = 0f
    private var pinchY = 0f
    private var widest = 1f
    private var tops = FloatArray(0)
    private var pageW = IntArray(0)
    private var pageH = IntArray(0)
    private var totalH = 0f
    private var laidFor = -1

    private val main = Handler(Looper.getMainLooper())

    /** A rendered page, or part of one at (x, y) in page pixels, at `scale` pixels per point (no
     *  bitmap if the engine failed on it), and the frame it was last wanted in. */
    private class Shot(val scale: Float, val x: Int, val y: Int, val w: Int, val h: Int, val bitmap: Bitmap?, var used: Long) {
        val bytes = if (bitmap != null) 4L * w * h else 0L
    }

    /** Whole pages, by page. */
    private val whole = HashMap<Int, Shot>()
    private var wholeBytes = 0L
    /** Zoomed in: the part of each page on screen, sharp. */
    private val details = HashMap<Int, Shot>()
    private var frame = 0L
    /** Whole pages rendered and on their way to the UI thread, so they aren't asked for again. */
    private val arriving = java.util.Collections.synchronizedSet(HashSet<Int>())
    private var asked = IntArray(0)
    private var askedLook = -1
    /** 1 while scrolling down, -1 up: the next page that way is rendered ahead. */
    private var ahead = 1
    /** When the pages last moved: sharp parts are only rendered once they stop. */
    private var lastMove = 0L
    // Reused by every frame.
    private val want = ArrayList<IntArray>()
    private val sharp = ArrayList<IntArray>()
    private val pageRect = RectF()
    private val partRect = RectF()
    @Volatile private var running = true

    /** Starts taking the pages of the PDF opened as `doc`. */
    fun start(doc: Int) {
        this.doc = doc
        Thread(::receive, "pdf-pages").apply { isDaemon = true; start() }
    }

    /** The PDF is closing; its thread ends when it does. */
    fun stop() {
        running = false
    }

    /** The theme changed: pages are drawn again in its colours. */
    fun recolor() {
        val p = Settings.pal
        val want = when {
            p.dark -> 1
            p.id == "WarmSepia" -> 2
            else -> 0
        }
        if (want != look) {
            look = want
            whole.clear()
            wholeBytes = 0
            details.clear()
        }
        invalidate()
    }

    private fun layoutPages() {
        if (width == 0 || sizes.isEmpty() || laidFor == width) return
        val anchor = pending ?: if (laidFor > 0) anchor() else 0f
        laidFor = width
        widest = sizes.maxOf { it.first }.coerceAtLeast(1f)
        fit = (width - 2 * gap) / widest
        scale = fit * zoom
        tops = FloatArray(sizes.size)
        pageW = IntArray(sizes.size)
        pageH = IntArray(sizes.size)
        var y = safeTop + gap
        for ((i, s) in sizes.withIndex()) {
            pageW[i] = (s.first * scale).roundToInt().coerceAtLeast(1)
            pageH[i] = (s.second * scale).roundToInt().coerceAtLeast(1)
            tops[i] = y
            y += pageH[i] + gap
        }
        totalH = y
        // Pages rendered at another size stay, stretched, until they're rendered again.
        pending = null
        scrollPos = yFor(anchor)
        clamp()
    }

    override fun onSizeChanged(w: Int, h: Int, ow: Int, oh: Int) {
        if (ow > 0 && pending == null) pending = anchor()
        laidFor = -1
        layoutPages()
    }

    private fun maxScroll() = max(0f, totalH - height)

    /** How far a zoomed page can pan: the widest page plus its margins, less the screen. */
    private fun maxPan() = max(0f, widest * scale + 2 * gap - width)

    private fun clamp() {
        scrollPos = scrollPos.coerceIn(0f, maxScroll())
        panX = panX.coerceIn(0f, maxPan())
    }

    /** Zooms to `z` (1 to 4), keeping the point under (fx, fy) where it is. */
    private fun setZoom(z: Float, fx: Float, fy: Float) {
        val nz = z.coerceIn(1f, 4f)
        if (kotlin.math.abs(nz - zoom) < 0.01f) return
        val ratio = nz / zoom
        val docY = scrollPos + fy
        val docX = panX + fx
        zoom = nz
        laidFor = -1
        pending = null
        layoutPages()
        scrollPos = docY * ratio - fy
        panX = docX * ratio - fx
        clamp()
        moved()
        invalidate()
        screen.positionChanged()
    }

    fun currentPage(): Int {
        if (tops.isEmpty()) return 0
        val mid = scrollPos + height / 3f
        val i = tops.indexOfLast { it <= mid }
        return i.coerceIn(0, tops.size - 1)
    }

    /** Page + fraction at the top of the screen. */
    fun anchor(): Float {
        if (tops.isEmpty()) return pending ?: 0f
        if (scrollPos >= maxScroll() - 1f && maxScroll() > 0) return sizes.size.toFloat()
        val y = scrollPos + safeTop + gap
        val i = tops.indexOfLast { it <= y }.coerceAtLeast(0)
        return i + ((y - tops[i]) / pageH[i].toFloat()).coerceIn(0f, 0.999f)
    }

    private fun yFor(anchor: Float): Float {
        if (tops.isEmpty()) return 0f
        val i = anchor.toInt().coerceIn(0, tops.size - 1)
        return tops[i] + (anchor - i).coerceIn(0f, 1f) * pageH[i] - safeTop - gap
    }

    fun atEnd(): Boolean = anchor() >= sizes.size - 0.05f

    fun goTo(anchor: Float) {
        if (tops.isEmpty()) {
            pending = anchor
            return
        }
        scrollPos = yFor(anchor)
        clamp()
        invalidate()
    }

    private fun scrollBy(dy: Float) {
        if (dy != 0f) ahead = if (dy > 0) 1 else -1
        scrollPos += dy
        clamp()
        moved()
        invalidate()
        screen.positionChanged()
    }

    private fun moved() {
        lastMove = android.os.SystemClock.uptimeMillis()
    }

    // ---- rendering ----------------------------------------------------------------------

    /** The scale page `i` is rendered at whole: the screen's width, or less for a page so big it
     *  would take more than MAX_PAGE_BYTES (its part on screen is then rendered sharp too). */
    private fun wholeScale(i: Int): Float {
        val (pw, ph) = sizes[i]
        return min(fit, kotlin.math.sqrt(MAX_PAGE_BYTES / (4f * pw * ph).coerceAtLeast(1f)))
    }

    /** A render job: page, column and row (the key the result comes back with), x, y, width and
     *  height in page pixels, and pixels per point. */
    private fun job(i: Int, x: Int, y: Int, w: Int, h: Int, s: Float) = intArrayOf(i, x / SNAP, y / SNAP, x, y, w, h, s.toRawBits())

    private fun wholeJob(i: Int): IntArray {
        val s = wholeScale(i)
        val (pw, ph) = sizes[i]
        return job(i, 0, 0, (pw * s).roundToInt().coerceAtLeast(1), (ph * s).roundToInt().coerceAtLeast(1), s)
    }

    /** The first page that reaches below `y`. */
    private fun firstPageBelow(y: Float): Int {
        var lo = 0
        var hi = tops.size
        while (lo < hi) {
            val mid = (lo + hi) ushr 1
            if (tops[mid] + pageH[mid] < y) lo = mid + 1 else hi = mid
        }
        return lo
    }

    /** Left edge of page `i` on screen, in whole pixels. Zoomed wider than the screen, pages line
     *  up on the widest and pan together. */
    private fun pageX(i: Int): Int {
        val wide = widest * scale + 2 * gap > width
        return (if (wide) gap + (widest * scale - pageW[i]) / 2 - panX else (width - pageW[i]) / 2f).roundToInt()
    }

    override fun onDraw(c: Canvas) {
        layoutPages()
        val p = Settings.pal
        c.drawColor(p.bg)
        if (tops.isEmpty()) return
        frame++
        if (pinch != 1f) {
            c.save()
            c.scale(pinch, pinch, pinchX, pinchY)
        }
        val first = firstPageBelow(scrollPos)
        var end = first
        while (end < tops.size && tops[end] <= scrollPos + height) end++
        want.clear()
        sharp.clear()
        val settled = android.os.SystemClock.uptimeMillis() - lastMove >= SETTLE_MS
        val r = pageRect
        for (i in first until end) {
            val x = pageX(i)
            val y = (tops[i] - scrollPos).roundToInt()
            r.set(x.toFloat(), y.toFloat(), (x + pageW[i]).toFloat(), (y + pageH[i]).toFloat())
            // The whole page, stretched if it was rendered at another size.
            val w = whole[i]
            w?.used = frame
            val bmp = w?.bitmap
            if (bmp != null) c.drawBitmap(bmp, null, r, paint) else {
                boxPaint.style = Paint.Style.FILL
                boxPaint.color = if (look == 1) 0xFF262626.toInt() else if (look == 2) 0xFFF7EED6.toInt() else 0xFFFFFFFF.toInt()
                c.drawRect(r, boxPaint)
            }
            val ws = wholeScale(i)
            if ((w == null || w.scale != ws) && i !in arriving) want.add(wholeJob(i))
            // Zoomed in past it: the part on screen, sharp (or from before the zoom, stretched).
            if (scale > ws * 1.01f) {
                val d = details[i]
                d?.used = frame
                val dbmp = d?.bitmap
                if (d != null && dbmp != null) {
                    val k = scale / d.scale
                    c.save()
                    c.clipRect(r)
                    partRect.set(x + d.x * k, y + d.y * k, x + (d.x + d.w) * k, y + (d.y + d.h) * k)
                    c.drawBitmap(dbmp, null, partRect, paint)
                    c.restore()
                }
                val vx0 = max(0, -x)
                val vx1 = min(pageW[i], width - x)
                val vy0 = max(0, -y)
                val vy1 = min(pageH[i], height - y)
                val covered = d != null && d.scale == scale && d.x <= vx0 && d.y <= vy0 && d.x + d.w >= vx1 && d.y + d.h >= vy1
                if (!covered && vx0 < vx1 && vy0 < vy1) {
                    if (settled) {
                        // A little taller than the screen, so a small scroll doesn't need it again.
                        val x0 = vx0 / SNAP * SNAP
                        val y0 = max(0, vy0 - height / 8) / SNAP * SNAP
                        val x1 = min(pageW[i], vx1)
                        val y1 = min(pageH[i], vy1 + height / 8)
                        sharp.add(job(i, x0, y0, x1 - x0, y1 - y0, scale))
                    } else postInvalidateDelayed(SETTLE_MS)
                }
            }
            boxPaint.style = Paint.Style.STROKE
            boxPaint.strokeWidth = context.dpf(1)
            boxPaint.color = p.border
            c.drawRect(r, boxPaint)
        }
        if (pinch != 1f) c.restore()
        // Sharp parts of pages that left the screen, or from before zooming back out, go.
        details.keys.retainAll { it in first until end && scale > wholeScale(it) * 1.01f }
        want.addAll(sharp)
        // The next page in the direction of scrolling, so it's there when it shows.
        val next = if (ahead > 0) end else first - 1
        if (next in tops.indices) {
            val w = whole[next]
            w?.used = frame
            if ((w == null || w.scale != wholeScale(next)) && next !in arriving) want.add(wholeJob(next))
        }
        ask(want)
    }

    /** Tells the render threads what's wanted now, most important first, if that changed. */
    private fun ask(want: List<IntArray>) {
        val jobs = IntArray(want.size * 8)
        for ((n, j) in want.withIndex()) System.arraycopy(j, 0, jobs, n * 8, 8)
        if (jobs.contentEquals(asked) && look == askedLook) return
        asked = jobs
        askedLook = look
        Core.pdfWant(doc, look, jobs)
    }

    /** Takes rendered pages from the engine until the PDF closes, and hands them to the UI thread. */
    private fun receive() {
        while (running) {
            val t = Core.pdfTile(doc) ?: return
            val head = t.copyOf(7)
            if (head[1] == 0 && head[2] == 0) arriving.add(head[0])
            val bmp = if (t.size > 7) runCatching {
                val b = Bitmap.createBitmap(t, 7, head[5], head[5], head[6], Bitmap.Config.ARGB_8888)
                // Kept on the GPU alone, instead of in memory and again as the GPU's copy.
                b.copy(Bitmap.Config.HARDWARE, false)?.also { b.recycle() } ?: b
            }.getOrNull() else null
            main.post { arrived(head, bmp) }
        }
    }

    private fun arrived(head: IntArray, bmp: Bitmap?) {
        val i = head[0]
        if (head[1] == 0 && head[2] == 0) arriving.remove(i)
        if (head[3] != look || i !in sizes.indices) return
        val s = Float.fromBits(head[4])
        val shot = Shot(s, head[1] * SNAP, head[2] * SNAP, head[5], head[6], bmp, frame)
        if (head[1] == 0 && head[2] == 0 && s == wholeScale(i)) {
            whole.put(i, shot)?.let { wholeBytes -= it.bytes }
            wholeBytes += shot.bytes
            evict()
        } else if (s == scale) {
            details[i] = shot
        } else return
        invalidate()
    }

    /** Drops the pages least recently wanted until they fit in a few screens; never the ones the
     *  last frame showed or wanted next. */
    private fun evict() {
        val budget = (2.5 * width * height * 4).toLong()
        if (wholeBytes <= budget) return
        for ((i, s) in whole.entries.filter { it.value.used < frame }.sortedBy { it.value.used }.map { it.key to it.value }) {
            if (wholeBytes <= budget) break
            whole.remove(i)
            wholeBytes -= s.bytes
        }
    }

    // ---- touch ----------------------------------------------------------------------------

    private val gestures = GestureDetector(context, object : GestureDetector.SimpleOnGestureListener() {
        override fun onDown(e: MotionEvent): Boolean {
            if (!scroller.isFinished) scroller.forceFinished(true)
            return true
        }

        // Confirmed, so a double-tap (zoom) doesn't also show and hide the bars.
        override fun onSingleTapConfirmed(e: MotionEvent): Boolean {
            screen.toggleChrome()
            return true
        }

        override fun onScroll(e1: MotionEvent?, e2: MotionEvent, dx: Float, dy: Float): Boolean {
            if (zoom > 1.01f) panX += dx
            scrollBy(dy)
            return true
        }

        override fun onDoubleTap(e: MotionEvent): Boolean {
            // Double-tap zooms in to twice the size, or back to the page's width.
            setZoom(if (zoom > 1.01f) 1f else 2f, e.x, e.y)
            return true
        }

        override fun onFling(e1: MotionEvent?, e2: MotionEvent, vx: Float, vy: Float): Boolean {
            if (vy != 0f) ahead = if (vy < 0) 1 else -1
            scroller.fling(0, scrollPos.toInt(), 0, -vy.toInt(), 0, 0, 0, maxScroll().toInt())
            postInvalidateOnAnimation()
            return true
        }
    })

    private val pinches = android.view.ScaleGestureDetector(context, object : android.view.ScaleGestureDetector.SimpleOnScaleGestureListener() {
        override fun onScale(d: android.view.ScaleGestureDetector): Boolean {
            pinch = (pinch * d.scaleFactor).coerceIn(1f / zoom, 4f / zoom)
            pinchX = d.focusX
            pinchY = d.focusY
            moved()
            invalidate()
            return true
        }

        override fun onScaleEnd(d: android.view.ScaleGestureDetector) {
            val z = zoom * pinch
            pinch = 1f
            setZoom(z, pinchX, pinchY)
            invalidate()
        }
    })

    override fun onTouchEvent(event: MotionEvent): Boolean {
        pinches.onTouchEvent(event)
        if (pinches.isInProgress) return true
        return gestures.onTouchEvent(event) || super.onTouchEvent(event)
    }

    override fun computeScroll() {
        if (scroller.computeScrollOffset()) {
            scrollPos = scroller.currY.toFloat()
            clamp()
            moved()
            screen.positionChanged()
            postInvalidateOnAnimation()
        }
    }

    override fun onInitializeAccessibilityNodeInfo(info: AccessibilityNodeInfo) {
        super.onInitializeAccessibilityNodeInfo(info)
        // TalkBack's scroll gestures move a screen at a time.
        info.isScrollable = true
        info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SCROLL_FORWARD)
        info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SCROLL_BACKWARD)
    }

    override fun performAccessibilityAction(action: Int, args: android.os.Bundle?): Boolean = when (action) {
        AccessibilityNodeInfo.ACTION_SCROLL_FORWARD -> { scrollBy(height * 0.85f); true }
        AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD -> { scrollBy(-height * 0.85f); true }
        else -> super.performAccessibilityAction(action, args)
    }
}

/** The most a whole rendered page takes (ARGB, 4 bytes a pixel): a tablet's screen width in
 *  portrait. Bigger pages are rendered smaller, and sharp where they're on screen. */
private const val MAX_PAGE_BYTES = 20 * 1024 * 1024
/** Sharp parts of zoomed pages start on multiples of this, which also names them. */
private const val SNAP = 64
/** How long the pages must be still before their sharp parts are rendered. */
private const val SETTLE_MS = 150L
