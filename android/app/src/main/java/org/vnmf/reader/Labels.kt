// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

package org.vnmf.reader

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import uniffi.vnmf_ffi.Book
import uniffi.vnmf_ffi.ChapterInfo
import uniffi.vnmf_ffi.VolumeInfo

/*
 * Volume and chapter labels in the app's language. The Rust core returns numbers and titles
 * from the file; the words ("Chapter", "Volume") come from the string resources.
 */

/** "Chapter 3 — Title" / "Chapter 10.5". */
@Composable
fun chapterLabel(chapter: ChapterInfo): String =
    chapter.title?.let { stringResource(R.string.chapter_titled, chapter.number, it) }
        ?: stringResource(R.string.chapter, chapter.number)

/** "Volume 1 — Title" / "Volume 1" / "No volume". */
@Composable
fun volumeLabel(volume: VolumeInfo): String = when {
    volume.number == 0u -> stringResource(R.string.no_volume)
    volume.title != null -> stringResource(R.string.volume_titled, volume.number.toInt(), volume.title!!)
    else -> stringResource(R.string.volume, volume.number.toInt())
}

/** "Volume 1 · Chapter 3 — Title" for a page; `null` — the book is a single chapter without volumes. */
@Composable
fun positionLabel(book: Book, layout: BookLayout, page: Int): String? {
    val position = book.position(page.toUInt()) ?: return null
    val volume = layout.contents.getOrNull(position.volume.toInt()) ?: return null
    val chapter = chapterLabel(volume.chapters.getOrNull(position.chapter.toInt()) ?: return null)
    if (!layout.hasVolumes) return chapter
    val shortVolume = if (volume.number == 0u) stringResource(R.string.no_volume) else stringResource(R.string.volume, volume.number.toInt())
    return stringResource(R.string.position, shortVolume, chapter)
}
