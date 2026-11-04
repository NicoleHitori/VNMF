// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

package org.vnmf.reader

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/** A recently opened book. */
data class RecentBook(val uri: Uri, val uuid: String, val title: String, val pages: Int, val openedAt: Long)

/** Where reading stopped: the page, and in strip mode also the fragment and the point within it (as on desktop). */
data class Position(val page: Int, val chunk: Int = 0, val offset: Float = 0f)

/**
 * Library: recent books (URIs with a persistent read permission), their covers
 * in app files, and reading positions — keyed by book UUID, so the position isn't lost if the file is moved.
 */
class Library(private val context: Context) {
    private val prefs = context.getSharedPreferences("library", Context.MODE_PRIVATE)
    private val coversDir = File(context.filesDir, "covers").apply { mkdirs() }

    fun recent(): List<RecentBook> {
        val array = JSONArray(prefs.getString("recent", "[]"))
        return (0 until array.length()).map { i ->
            val o = array.getJSONObject(i)
            RecentBook(Uri.parse(o.getString("uri")), o.getString("uuid"), o.getString("title"), o.optInt("pages"), o.optLong("openedAt"))
        }
    }

    /** Remember an opened book (at the top of the list) and its cover. */
    fun remember(uri: Uri, uuid: String, title: String, pages: Int, cover: ByteArray?) {
        // Persistent permission: the URI keeps working after the phone restarts.
        runCatching { context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION) }
        val list = recent().filter { it.uuid != uuid && it.uri != uri }.toMutableList()
        list.add(0, RecentBook(uri, uuid, title, pages, System.currentTimeMillis()))
        save(list.take(200))
        val file = File(coversDir, "$uuid.png")
        if (cover != null && !file.exists()) {
            Images.decode("cover-file:$uuid", cover, 360, hardware = false)?.let { bitmap ->
                file.outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
            }
        }
    }

    fun forget(book: RecentBook) {
        save(recent().filter { it.uuid != book.uuid })
        File(coversDir, "${book.uuid}.png").delete()
        runCatching { context.contentResolver.releasePersistableUriPermission(book.uri, Intent.FLAG_GRANT_READ_URI_PERMISSION) }
    }

    fun cover(uuid: String): Bitmap? = File(coversDir, "$uuid.png").takeIf { it.exists() }?.let { BitmapFactory.decodeFile(it.path) }

    private fun save(list: List<RecentBook>) {
        val array = JSONArray()
        list.forEach {
            array.put(JSONObject().put("uri", it.uri.toString()).put("uuid", it.uuid).put("title", it.title).put("pages", it.pages).put("openedAt", it.openedAt))
        }
        prefs.edit().putString("recent", array.toString()).apply()
    }

    fun position(uuid: String): Position? {
        val raw = prefs.getString("pos:$uuid", null) ?: return null
        val o = JSONObject(raw)
        return Position(o.getInt("page"), o.optInt("chunk"), o.optDouble("offset", 0.0).toFloat())
    }

    fun setPosition(uuid: String, position: Position) {
        val o = JSONObject().put("page", position.page).put("chunk", position.chunk).put("offset", position.offset.toDouble())
        prefs.edit().putString("pos:$uuid", o.toString()).apply()
    }

    /** Mode chosen manually for the book: `true` — strip, `false` — paged, `null` — as in the file. */
    fun stripMode(uuid: String): Boolean? = if (prefs.contains("strip:$uuid")) prefs.getBoolean("strip:$uuid", false) else null

    fun setStripMode(uuid: String, strip: Boolean) = prefs.edit().putBoolean("strip:$uuid", strip).apply()

    /** Forget all saved reading positions (the library stays). */
    fun forgetPositions() {
        val editor = prefs.edit()
        prefs.all.keys.filter { it.startsWith("pos:") }.forEach { editor.remove(it) }
        editor.apply()
    }
}
