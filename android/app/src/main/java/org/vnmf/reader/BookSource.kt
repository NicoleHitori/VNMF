// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

package org.vnmf.reader

import android.content.Context
import android.net.Uri
import uniffi.vnmf_ffi.Book

/** Opens a book from a `content://` URI: the descriptor is handed to Rust (no file copy). */
object BookSource {
    fun open(context: Context, uri: Uri): Book {
        val descriptor = context.contentResolver.openFileDescriptor(uri, "r")
            ?: throw IllegalStateException("Could not open the file")
        // detachFd: the descriptor now belongs to Rust — it will close it.
        return Book.openFd(descriptor.detachFd())
    }

    /** File name to display when the metadata has no title. */
    fun displayName(context: Context, uri: Uri): String {
        context.contentResolver.query(uri, arrayOf(android.provider.OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) {
                c.getString(0)?.let { return it.removeSuffix(".vnmf") }
            }
        }
        return uri.lastPathSegment?.substringAfterLast('/')?.removeSuffix(".vnmf") ?: "Untitled"
    }
}
