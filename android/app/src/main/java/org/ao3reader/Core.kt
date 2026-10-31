package org.ao3reader

import android.os.Handler
import android.os.Looper
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.Executors

class CoreError(message: String) : Exception(message)

/**
 * The Rust core (libao3core.so). Every call is `method` + JSON arguments → JSON result.
 * Quick calls run on the UI thread; network calls go through [background].
 */
object Core {
    init {
        System.loadLibrary("ao3core")
    }

    @JvmStatic
    private external fun call(method: String, arg: String): String

    /** The parts of the open PDF's pages (`doc` from "pdf.open") to render, most important first,
     *  in place of the ones asked for before: (page, column, row, x, y, width, height, scale as
     *  float bits) each, the column and row naming the part, x to height in page pixels at the
     *  scale (pixels per point); `look` 0 is as printed, 1 dark, 2 sepia. Returns at once. */
    @JvmStatic
    external fun pdfWant(doc: Int, look: Int, jobs: IntArray)

    /** Waits for the next rendered part: (page, column, row, look, scale as float bits, width,
     *  height), then its ARGB pixels (none if the engine failed on it). Null once the PDF is
     *  closed. Blocks: call it on a thread of its own. */
    @JvmStatic
    external fun pdfTile(doc: Int): IntArray?

    // A few at once, so a word lookup or a sign-in doesn't wait behind a long download.
    private val worker = Executors.newFixedThreadPool(3)
    /** Long batches (checking followed feeds) run here so they don't hold up downloads. */
    private val slowWorker = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())

    fun req(method: String, arg: JSONObject = JSONObject()): Any? {
        val res = JSONObject(call(method, arg.toString()))
        if (res.has("err")) throw CoreError(res.getString("err"))
        return res.opt("ok")
    }

    fun obj(method: String, arg: JSONObject = JSONObject()): JSONObject = req(method, arg) as JSONObject
    fun arr(method: String, arg: JSONObject = JSONObject()): JSONArray = req(method, arg) as JSONArray

    /** Runs a (possibly slow, network) call off the UI thread and delivers the result on it. */
    fun background(method: String, arg: JSONObject, slow: Boolean = false, done: (Result<Any?>) -> Unit) {
        (if (slow) slowWorker else worker).execute {
            val r = runCatching { req(method, arg) }
            main.post { done(r) }
        }
    }
}

fun json(vararg pairs: Pair<String, Any?>): JSONObject = JSONObject().apply { pairs.forEach { (k, v) -> put(k, v) } }

fun JSONArray.objects(): List<JSONObject> = (0 until length()).map { getJSONObject(it) }
fun JSONArray.strings(): List<String> = (0 until length()).map { getString(it) }
