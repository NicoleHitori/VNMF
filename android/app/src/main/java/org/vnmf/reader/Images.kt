// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

package org.vnmf.reader

import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.util.LruCache
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.ensureActive
import uniffi.vnmf_ffi.decodeAvif
import java.nio.ByteBuffer

/**
 * Page images: decoding, cache and queue.
 *
 * - **Downscaling during decoding** to the screen width: a long webtoon fragment at full
 *   resolution would take tens of megabytes.
 * - **Hardware bitmaps** (pixels go straight to video memory): the first display doesn't upload 5–10 MB to the GPU
 *   mid-frame — the main cause of stutter while paging. Very tall ones (over [MAX_HARDWARE]) are
 *   software bitmaps, but they are uploaded to the GPU in advance ([Bitmap.prepareToDraw]).
 * - **Bounded queue with cancellation**: [WORKERS] images are decoded at once; if nobody is waiting
 *   for an image anymore (scrolled past quickly), its decoding is cancelled instead of occupying the queue —
 *   otherwise visible pages would wait behind dozens of skipped ones.
 * - **No duplication**: an on-screen row and a prefetch asking for the same image wait for a single decode.
 */
object Images {
    /** How many images to decode at once: more only fights the UI for cores. */
    private const val WORKERS = 3

    /** A taller hardware texture may not be supported by the GPU — then a software bitmap. */
    private const val MAX_HARDWARE = 4096

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO.limitedParallelism(WORKERS))

    /** Cache: key — (chunk, width, dark version). Size — in pixel bytes, a quarter of the app's memory. */
    private val cache = object : LruCache<String, Bitmap>((Runtime.getRuntime().maxMemory() / 4).toInt()) {
        override fun sizeOf(key: String, value: Bitmap) = value.allocationByteCount
    }

    private class Pending(val job: Deferred<Bitmap?>) {
        var waiters = 0
    }

    private val pending = HashMap<String, Pending>()

    fun cached(key: String): Bitmap? = cache.get(key)

    /**
     * Image from the cache or the decode queue. Cancelling the waiter (row left the screen)
     * also cancels the decode if nobody else is waiting for it. `bytes` are read already in the queue.
     */
    suspend fun load(key: String, targetWidth: Int, bytes: () -> ByteArray): Bitmap? {
        cache.get(key)?.let { return it }
        val entry = synchronized(pending) {
            pending.getOrPut(key) {
                Pending(scope.async {
                    ensureActive() // cancelled while waiting in the queue — don't decode
                    decode(key, bytes(), targetWidth)
                })
            }.also { it.waiters++ }
        }
        try {
            return entry.job.await()
        } finally {
            synchronized(pending) {
                entry.waiters--
                if (entry.waiters == 0) {
                    if (pending[key] === entry) pending.remove(key)
                    if (!entry.job.isCompleted) entry.job.cancel()
                }
            }
        }
    }

    /**
     * Decodes immediately (on the caller's thread), fitting into `targetWidth` px width (never upscales). `null` — failed.
     * `hardware = false` — a software bitmap (it can be compressed to a file, e.g. a library cover).
     */
    fun decode(key: String, bytes: ByteArray, targetWidth: Int, hardware: Boolean = true): Bitmap? {
        cache.get(key)?.let { return it }
        val bitmap = (if (hardware) system(bytes, targetWidth, hardware = true) else null)
            ?: system(bytes, targetWidth, hardware = false)
            ?: if (isAvif(bytes)) avifBitmap(bytes, targetWidth, hardware) else null
        bitmap?.let { cache.put(key, it) }
        return bitmap
    }

    fun clear() = cache.evictAll()

    /** The system `ImageDecoder` (PNG, JPEG, WebP, GIF; AVIF — if the phone supports it). */
    private fun system(bytes: ByteArray, targetWidth: Int, hardware: Boolean): Bitmap? = try {
        var software = !hardware
        val bitmap = ImageDecoder.decodeBitmap(ImageDecoder.createSource(ByteBuffer.wrap(bytes))) { decoder, info, _ ->
            val w = info.size.width
            var h = info.size.height
            if (targetWidth in 1 until w) {
                h = (h.toLong() * targetWidth / w).toInt().coerceAtLeast(1)
                decoder.setTargetSize(targetWidth, h)
            }
            if (h > MAX_HARDWARE) software = true
            decoder.allocator = if (software) ImageDecoder.ALLOCATOR_SOFTWARE else ImageDecoder.ALLOCATOR_HARDWARE
        }
        // Software bitmap — into video memory in advance, in the background, not during the first frame.
        if (software) bitmap.prepareToDraw()
        bitmap
    } catch (_: Exception) {
        null
    }

    /**
     * AVIF with our own decoder (Rust, `vnmf-ffi`): the system one on Android 12–13 often can't open
     * 4:4:4 (AV1 High profile), 10/12-bit, etc. — the page stayed blank.
     */
    private fun avifBitmap(bytes: ByteArray, targetWidth: Int, toHardware: Boolean): Bitmap? = try {
        val image = decodeAvif(bytes, targetWidth.coerceAtLeast(0).toUInt())
        val software = Bitmap.createBitmap(image.width.toInt(), image.height.toInt(), Bitmap.Config.ARGB_8888).apply {
            copyPixelsFromBuffer(ByteBuffer.wrap(image.pixels))
        }
        val hardware = if (toHardware && software.height <= MAX_HARDWARE) software.copy(Bitmap.Config.HARDWARE, false) else null
        if (hardware != null) {
            software.recycle()
            hardware
        } else {
            software.apply { prepareToDraw() }
        }
    } catch (_: Exception) {
        null
    }

    /** ISO-BMFF with the `avif`/`avis` brand (major or among compatible ones) — like `is_avif` in vnmf-core. */
    private fun isAvif(bytes: ByteArray): Boolean {
        if (bytes.size < 16 || String(bytes, 4, 4, Charsets.ISO_8859_1) != "ftyp") return false
        val boxSize = ((bytes[0].toInt() and 0xFF) shl 24) or ((bytes[1].toInt() and 0xFF) shl 16) or
            ((bytes[2].toInt() and 0xFF) shl 8) or (bytes[3].toInt() and 0xFF)
        val end = boxSize.coerceIn(16, bytes.size)
        val brand = { at: Int -> String(bytes, at, 4, Charsets.ISO_8859_1).let { it == "avif" || it == "avis" } }
        return brand(8) || (16 until end - 3 step 4).any(brand)
    }
}
