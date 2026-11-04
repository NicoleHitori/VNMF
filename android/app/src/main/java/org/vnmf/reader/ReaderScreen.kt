// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

package org.vnmf.reader

import android.app.Activity
import android.graphics.Bitmap
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.rememberTransformableState
import androidx.compose.foundation.gestures.transformable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.automirrored.filled.Notes
import androidx.compose.material.icons.automirrored.filled.VolumeOff
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material.icons.filled.AutoStories
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.ViewDay
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.vnmf_ffi.Book
import uniffi.vnmf_ffi.BookInfo
import uniffi.vnmf_ffi.PageInfo
import kotlin.math.roundToInt

/** A fragment in reading order: chunk index in the file, the page, and the fragment number within it. */
data class ChunkRef(val index: UInt, val page: Int, val part: Int, val width: Int, val height: Int) {
    val ratio: Float get() = width.toFloat() / height.coerceAtLeast(1)
}

/**
 * Book layout: pages, fragments in reading order, and contents. Computed once on
 * opening, on a background thread (Rust calls and walking all pages — not on the main thread).
 */
class BookLayout(book: Book) {
    val pages: List<PageInfo> = book.pages()
    val chunks: List<ChunkRef> =
        pages.flatMap { p -> p.chunks.mapIndexed { k, c -> ChunkRef(c.index, p.index.toInt(), k, c.width.toInt(), c.height.toInt()) } }
    /** Fragments of each page (index — page number): no search through the whole list on every frame. */
    val byPage: List<List<ChunkRef>> = chunks.groupBy { it.page }.let { grouped -> pages.indices.map { grouped[it].orEmpty() } }
    /** The first strip row of each page. */
    val firstRow: IntArray = IntArray(pages.size).also { rows ->
        var row = 0
        byPage.forEachIndexed { i, parts -> rows[i] = row; row += parts.size }
    }
    val contents = book.contents()
}

/** Cache key of a fragment image: the same width and version (dark or regular) — the same bitmap. */
private fun chunkKey(chunk: ChunkRef, width: Int, dark: Boolean) = "${chunk.index}:$width:$dark"

/** Fragment bytes (the dark version if needed and available). Called already in the decode queue. */
private fun chunkBytes(book: Book, chunk: ChunkRef, dark: Boolean): ByteArray =
    (if (dark) book.darkChunk(chunk.page.toUInt(), chunk.part.toUInt()) else null) ?: book.chunk(chunk.index)

/** How long the reader bar stays visible when untouched (the "Hide the reader bar" setting). */
private const val AUTO_HIDE_MS = 3500L

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ReaderScreen(
    book: Book,
    info: BookInfo,
    layout: BookLayout,
    title: String,
    library: Library,
    settings: ReaderSettings,
    onClose: () -> Unit,
    onSettings: () -> Unit,
) {
    val context = LocalContext.current
    val pages = layout.pages
    val chunks = layout.chunks
    val contents = layout.contents
    val dark = isSystemInDarkTheme() && info.hasDark && settings.darkPages
    val sound = remember(book) { Sound(context, book) }
    var current by remember { mutableIntStateOf(0) }
    var chrome by remember { mutableStateOf(false) }
    var showContents by remember { mutableStateOf(false) }
    var showText by remember { mutableStateOf(false) }
    val saved = remember(book) { if (settings.rememberPosition) library.position(info.uuid) else null }
    // Mode: strip (webtoon) or paged. The default comes from the file; a manual choice is remembered per book
    // (manga as a strip is handy on tall screens).
    var strip by remember(book) { mutableStateOf(library.stripMode(info.uuid) ?: info.webtoon) }
    // Where to start: on opening — the saved position, after switching — the current page.
    var start by remember(book) { mutableStateOf(saved) }
    val savePosition: (Position) -> Unit = { if (settings.rememberPosition) library.setPosition(info.uuid, it) }

    DisposableEffect(book) { onDispose { sound.release() } }
    BackHandler(onBack = onClose)
    Immersive(visible = chrome || !settings.immersive)

    // The bar disappears by itself (if configured) — the countdown restarts after every show.
    LaunchedEffect(chrome, settings.autoHide) {
        if (chrome && settings.autoHide) {
            delay(AUTO_HIDE_MS)
            chrome = false
        }
    }
    // Music follows the current page; sound and volume come from the settings.
    LaunchedEffect(current, settings.sound) {
        sound.enabled = settings.sound
        sound.setMusic(withContext(Dispatchers.IO) { book.musicAt(current.toUInt()) })
    }
    LaunchedEffect(settings.volume) { sound.masterVolume = settings.volume / 100f }

    val background = settings.background.color ?: MaterialTheme.colorScheme.background
    Box(Modifier.fillMaxSize().background(background)) {
        // key: after switching, the mode is rebuilt from the current position.
        androidx.compose.runtime.key(strip) {
            if (strip) {
                Strip(book, layout, dark, settings, start, sound, savePosition, onCurrent = { current = it }, onTap = { chrome = !chrome })
            } else {
                Paged(book, layout, info.rtl, dark, settings, start, sound, savePosition, onCurrent = { current = it }, onTap = { chrome = !chrome })
            }
        }

        AnimatedVisibility(chrome, enter = fadeIn(), exit = fadeOut()) {
            TopAppBar(
                title = {
                    Column {
                        Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        val position = book.positionLabel(current.toUInt())
                        Text(
                            (position?.let { "$it · " } ?: "") + "page ${current + 1} of ${pages.size}",
                            style = MaterialTheme.typography.bodySmall,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                },
                navigationIcon = { IconButton(onClick = onClose) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                actions = {
                    if (info.hasText) IconButton(onClick = { showText = true }) { Icon(Icons.AutoMirrored.Filled.Notes, "Page text") }
                    if (info.hasMusic || info.hasSfx) {
                        IconButton(onClick = { settings.sound = !settings.sound }) {
                            Icon(if (settings.sound) Icons.AutoMirrored.Filled.VolumeUp else Icons.AutoMirrored.Filled.VolumeOff, "Sound")
                        }
                    }
                    IconButton(onClick = {
                        start = Position(current)
                        sound.clearEffects()
                        strip = !strip
                        library.setStripMode(info.uuid, strip)
                    }) {
                        if (strip) Icon(Icons.Filled.AutoStories, "Read page by page") else Icon(Icons.Filled.ViewDay, "Read as a strip")
                    }
                    if (contents.sumOf { it.chapters.size } > 1) {
                        IconButton(onClick = { showContents = true }) { Icon(Icons.AutoMirrored.Filled.List, "Contents") }
                    }
                    IconButton(onClick = onSettings) { Icon(Icons.Filled.Settings, "Settings") }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.surface.copy(alpha = 0.92f)),
            )
        }
    }

    if (showContents) {
        ModalBottomSheet(onDismissRequest = { showContents = false }) {
            LazyColumn {
                contents.forEach { volume ->
                    if (volume.label.isNotEmpty()) {
                        item { Text(volume.label, style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(16.dp, 12.dp)) }
                    }
                    volume.chapters.forEach { chapter ->
                        item {
                            ListItem(
                                headlineContent = { Text(chapter.label) },
                                supportingContent = { Text("${chapter.pageCount} pp.") },
                                modifier = Modifier.clickable {
                                    showContents = false
                                    JumpBus.request(chapter.firstPage.toInt())
                                },
                            )
                        }
                    }
                }
            }
        }
    }

    if (showText) {
        ModalBottomSheet(onDismissRequest = { showText = false }) {
            val text = remember(current) { runCatching { book.pageText(current.toUInt()) }.getOrNull() }
            Text(
                text ?: "There is no text on this page.",
                modifier = Modifier.padding(20.dp).navigationBarsPadding().verticalScroll(rememberScrollState()),
            )
        }
    }
}

/** Navigation from the contents: the reader picks it up and scrolls. */
object JumpBus {
    var target by mutableStateOf<Int?>(null)
        private set

    fun request(page: Int) {
        target = page
    }

    fun consume(): Int? = target.also { target = null }
}

/** Fullscreen reading: system bars are hidden when `visible = false`. */
@Composable
private fun Immersive(visible: Boolean) {
    val view = LocalView.current
    val window = (view.context as? Activity)?.window ?: return
    LaunchedEffect(visible) {
        val controller = WindowCompat.getInsetsController(window, view)
        controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        if (visible) controller.show(WindowInsetsCompat.Type.systemBars()) else controller.hide(WindowInsetsCompat.Type.systemBars())
    }
    DisposableEffect(Unit) {
        onDispose { WindowCompat.getInsetsController(window, view).show(WindowInsetsCompat.Type.systemBars()) }
    }
}

/**
 * A fragment image. `fit` — fit into the available space (the whole page; `alignment` — which
 * edge to stick to so the spread halves join); otherwise — full width with the aspect ratio,
 * whose space is reserved right away, even before decoding.
 */
@Composable
fun ChunkImage(
    book: Book,
    chunk: ChunkRef,
    dark: Boolean,
    targetWidth: Int,
    modifier: Modifier = Modifier,
    fit: Boolean = false,
    alignment: Alignment = Alignment.Center,
) {
    val key = chunkKey(chunk, targetWidth, dark)
    var bitmap by remember(key) { mutableStateOf<Bitmap?>(Images.cached(key)) }
    LaunchedEffect(key) {
        // A bounded queue with cancellation: left the screen without waiting — decoding is cancelled.
        if (bitmap == null) bitmap = Images.load(key, targetWidth) { chunkBytes(book, chunk, dark) }
    }
    if (fit) {
        Box(modifier) {
            bitmap?.let { Image(it.asImageBitmap(), null, Modifier.fillMaxSize(), alignment = alignment, contentScale = ContentScale.Fit) }
        }
    } else {
        Box(modifier.aspectRatio(chunk.ratio).background(Color(0xFF1C1B1F))) {
            bitmap?.let { Image(it.asImageBitmap(), null, Modifier.fillMaxSize(), contentScale = ContentScale.FillBounds) }
        }
    }
}

// -------------------------------------------------------------------------------------------
// Webtoon: strip
// -------------------------------------------------------------------------------------------

@OptIn(FlowPreview::class)
@Composable
private fun Strip(
    book: Book,
    layout: BookLayout,
    dark: Boolean,
    settings: ReaderSettings,
    saved: Position?,
    sound: Sound,
    savePosition: (Position) -> Unit,
    onCurrent: (Int) -> Unit,
    onTap: () -> Unit,
) {
    val chunks = layout.chunks
    val state = rememberLazyListState()
    val effects = remember(book) { book.stripSfx() }
    val inside = remember { mutableSetOf<UInt>() }
    val share = settings.stripWidth.coerceIn(30, 100) / 100f
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val widthPx = (constraints.maxWidth * share).roundToInt()
        val heightPx = constraints.maxHeight
        val rowHeight by rememberUpdatedState { i: Int -> (widthPx / chunks[i].ratio).roundToInt() }

        // Restore the position: the page fragment and the point within it at the top edge (as on desktop).
        LaunchedEffect(Unit) {
            val pos = saved ?: return@LaunchedEffect
            val first = layout.firstRow.getOrNull(pos.page) ?: return@LaunchedEffect
            val row = (first + pos.chunk).coerceIn(chunks.indices)
            state.scrollToItem(row, (rowHeight(row) * pos.offset).roundToInt())
        }
        LaunchedEffect(Unit) {
            snapshotFlow { JumpBus.target }.collect {
                val page = JumpBus.consume() ?: return@collect
                layout.firstRow.getOrNull(page)?.let { state.scrollToItem(it) }
            }
        }
        // The current page (at a third of the height) and the strip's sound effects.
        LaunchedEffect(Unit) {
            snapshotFlow { state.firstVisibleItemIndex to state.firstVisibleItemScrollOffset }.collect {
                val visible = state.layoutInfo.visibleItemsInfo
                visible.firstOrNull { it.offset + it.size > heightPx / 3 }?.let { onCurrent(chunks[it.index].page) }
                for (sfx in effects) {
                    val item = visible.firstOrNull { it.index == sfx.chunk.toInt() }
                    val y = item?.let { it.offset + it.size * sfx.fraction }
                    val now = y != null && y >= 0 && y <= heightPx * 0.85
                    if (now && inside.add(sfx.aux.index)) sound.playEffect(sfx.aux)
                    if (!now && inside.remove(sfx.aux.index)) sound.leaveEffect(sfx.aux)
                }
            }
        }
        // Reading position — once scrolling has settled.
        LaunchedEffect(Unit) {
            snapshotFlow { state.firstVisibleItemIndex to state.firstVisibleItemScrollOffset }
                .distinctUntilChanged()
                .debounce(400)
                .collect { (row, offset) ->
                    val chunk = chunks.getOrNull(row) ?: return@collect
                    savePosition(Position(chunk.page, row - layout.firstRow[chunk.page], offset.toFloat() / rowHeight(row).coerceAtLeast(1)))
                }
        }
        // Preloading: the next rows (and a few previous ones) are decoded while you read the current ones,
        // — so there are no blank spots when scrolling. A new visible range cancels unfinished work.
        LaunchedEffect(widthPx, dark, settings.preload) {
            val ahead = (settings.preload * 2).coerceAtLeast(2)
            snapshotFlow {
                val visible = state.layoutInfo.visibleItemsInfo
                (visible.firstOrNull()?.index ?: 0) to (visible.lastOrNull()?.index ?: 0)
            }
                .distinctUntilChanged()
                .collectLatest { (first, last) ->
                    for (i in ((last + 1)..(last + ahead)) + ((first - 1) downTo (first - ahead / 2))) {
                        val chunk = chunks.getOrNull(i) ?: continue
                        Images.load(chunkKey(chunk, widthPx, dark), widthPx) { chunkBytes(book, chunk, dark) }
                    }
                }
        }

        LazyColumn(
            state = state,
            verticalArrangement = Arrangement.spacedBy(settings.stripGap.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier.fillMaxSize().pointerInput(Unit) { detectTapGestures(onTap = { onTap() }) },
        ) {
            itemsIndexed(chunks, key = { _, c -> c.index.toLong() }) { _, chunk ->
                ChunkImage(book, chunk, dark, widthPx, Modifier.fillMaxWidth(share))
            }
        }
    }
}

// -------------------------------------------------------------------------------------------
// Manga: paged
// -------------------------------------------------------------------------------------------

@Composable
private fun Paged(
    book: Book,
    layout: BookLayout,
    rtl: Boolean,
    dark: Boolean,
    settings: ReaderSettings,
    saved: Position?,
    sound: Sound,
    savePosition: (Position) -> Unit,
    onCurrent: (Int) -> Unit,
    onTap: () -> Unit,
) {
    val pages = layout.pages
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val widthPx = constraints.maxWidth
        // Spread — only in landscape orientation (on a narrow screen two pages would be tiny).
        val double = settings.doublePage && constraints.maxWidth > constraints.maxHeight
        val groups = remember(book, double, settings.coverAlone, rtl) { book.pageGroups(double, settings.coverAlone, rtl) }
        // The page to start from (and to stay on after rotating the screen or changing settings).
        var anchor by remember { mutableIntStateOf((saved?.page ?: 0).coerceIn(0, (pages.size - 1).coerceAtLeast(0))) }
        val pager = rememberPagerState(
            initialPage = groups.indexOfFirst { anchor.toUInt() in it.pages }.coerceAtLeast(0),
        ) { groups.size }
        val scope = rememberCoroutineScope()
        var zoomed by remember { mutableStateOf(false) }

        LaunchedEffect(groups) {
            val target = groups.indexOfFirst { anchor.toUInt() in it.pages }.coerceAtLeast(0)
            if (pager.currentPage != target) pager.scrollToPage(target)
        }
        LaunchedEffect(Unit) {
            snapshotFlow { JumpBus.target }.collect {
                val page = JumpBus.consume() ?: return@collect
                groups.indexOfFirst { page.toUInt() in it.pages }.takeIf { it >= 0 }?.let { pager.scrollToPage(it) }
            }
        }
        LaunchedEffect(pager.currentPage, groups) {
            val group = groups.getOrNull(pager.currentPage) ?: return@LaunchedEffect
            val first = group.pages.first().toInt()
            anchor = first
            onCurrent(first)
            savePosition(Position(first))
            sound.clearEffects()
            withContext(Dispatchers.IO) { group.pages.flatMap { book.sfxOf(it) } }.forEach { sound.playEffect(it) }
        }

        fun turn(delta: Int) = scope.launch { pager.animateScrollToPage((pager.currentPage + delta).coerceIn(0, groups.size - 1)) }

        HorizontalPager(
            state = pager,
            reverseLayout = rtl,
            userScrollEnabled = !zoomed,
            beyondViewportPageCount = settings.preload.coerceIn(0, 6),
            key = { groups[it].pages.first().toInt() },
            modifier = Modifier.fillMaxSize(),
        ) { index ->
            val group = groups[index]
            ZoomablePage(onZoomChange = { zoomed = it }, onTap = { x ->
                // Screen edges turn pages (taking reading direction into account), the middle toggles the bar.
                val third = widthPx / 3f
                when {
                    !settings.tapZones -> onTap()
                    x < third -> turn(if (rtl) 1 else -1)
                    x > widthPx - third -> turn(if (rtl) -1 else 1)
                    else -> onTap()
                }
            }) {
                val single = group.pages.singleOrNull()?.toInt()
                val parts = single?.let { layout.byPage[it] }
                when {
                    // A long page made of several fragments — scroll down.
                    parts != null && parts.size > 1 -> Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                        parts.forEach { ChunkImage(book, it, dark, widthPx, Modifier.fillMaxWidth()) }
                    }
                    // By width: the page (spread) at full width, then scroll down.
                    settings.fit == Fit.Width -> Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                        Row(Modifier.fillMaxWidth()) {
                            group.visual.forEach { p ->
                                layout.byPage[p.toInt()].firstOrNull()?.let {
                                    ChunkImage(book, it, dark, widthPx / group.visual.size, Modifier.weight(it.ratio))
                                }
                            }
                        }
                    }
                    // Whole: the spread halves are pressed against each other in the middle.
                    else -> Row(Modifier.fillMaxSize()) {
                        group.visual.forEachIndexed { i, p ->
                            layout.byPage[p.toInt()].firstOrNull()?.let {
                                val align = when {
                                    group.visual.size == 1 -> Alignment.Center
                                    i == 0 -> Alignment.CenterEnd
                                    else -> Alignment.CenterStart
                                }
                                ChunkImage(book, it, dark, widthPx / group.visual.size, Modifier.weight(1f).fillMaxHeight(), fit = true, alignment = align)
                            }
                        }
                    }
                }
            }
        }
    }
}

/** A zoomable page: pinch or double tap (×2.5), dragging when zoomed in. */
@Composable
private fun ZoomablePage(onZoomChange: (Boolean) -> Unit, onTap: (Float) -> Unit, content: @Composable () -> Unit) {
    // The gesture outlives a single recomposition — always call the fresh handler.
    val tap by rememberUpdatedState(onTap)
    var scale by remember { mutableFloatStateOf(1f) }
    var offsetX by remember { mutableFloatStateOf(0f) }
    var offsetY by remember { mutableFloatStateOf(0f) }
    val transform = rememberTransformableState { zoom, pan, _ ->
        scale = (scale * zoom).coerceIn(1f, 5f)
        if (scale == 1f) {
            offsetX = 0f
            offsetY = 0f
        } else {
            offsetX += pan.x
            offsetY += pan.y
        }
        onZoomChange(scale > 1f)
    }
    Box(
        Modifier
            .fillMaxSize()
            // A zoomed page doesn't overlap the neighbours (the pager doesn't clip pages itself).
            .clipToBounds()
            .pointerInput(Unit) {
                detectTapGestures(
                    onDoubleTap = {
                        scale = if (scale > 1f) 1f else 2.5f
                        offsetX = 0f
                        offsetY = 0f
                        onZoomChange(scale > 1f)
                    },
                    onTap = { tap(it.x) },
                )
            }
            .transformable(transform, canPan = { scale > 1f })
            .graphicsLayer {
                scaleX = scale
                scaleY = scale
                translationX = offsetX
                translationY = offsetY
            },
    ) { content() }
}
