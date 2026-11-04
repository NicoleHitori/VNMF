// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

package org.vnmf.reader

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.vnmf_ffi.Book
import uniffi.vnmf_ffi.BookInfo

/** An open book: the Rust object, info, layout (pages, fragments, contents) and the display title. */
class OpenBook(val book: Book, val info: BookInfo, val layout: BookLayout, val title: String)

class MainActivity : ComponentActivity() {
    /** A file passed in by another app (file manager, messenger). */
    private var incoming by mutableStateOf<Uri?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        incoming = intent?.viewedUri()
        setContent {
            val context = LocalContext.current
            val dark = isSystemInDarkTheme()
            val colors = if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
            MaterialTheme(colorScheme = colors) {
                App(incoming = incoming, onIncomingHandled = { incoming = null })
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        intent.viewedUri()?.let { incoming = it }
    }

    private fun Intent.viewedUri(): Uri? = data?.takeIf { action == Intent.ACTION_VIEW }
}

@Composable
private fun App(incoming: Uri?, onIncomingHandled: () -> Unit) {
    val context = LocalContext.current
    val library = remember { Library(context) }
    val settings = remember { ReaderSettings(context) }
    var showSettings by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    var open by remember { mutableStateOf<OpenBook?>(null) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    fun openUri(uri: Uri) {
        loading = true
        scope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    val book = BookSource.open(context, uri)
                    val info = book.info()
                    val title = info.title ?: BookSource.displayName(context, uri)
                    library.remember(uri, info.uuid, title, info.pageCount.toInt(), book.mainCover())
                    OpenBook(book, info, BookLayout(book), title)
                }
            }
            loading = false
            result.onSuccess { open?.book?.close(); open = it }.onFailure { error = it.message ?: it.toString() }
        }
    }

    LaunchedEffect(incoming) {
        incoming?.let {
            openUri(it)
            onIncomingHandled()
        }
    }

    val current = open
    when {
        // Settings — on top of the reader or library (the book stays open).
        showSettings -> SettingsScreen(settings, library, onBack = { showSettings = false })
        current != null -> ReaderScreen(
            current.book, current.info, current.layout, current.title, library, settings,
            onClose = {
                current.book.close()
                Images.clear()
                open = null
            },
            onSettings = { showSettings = true },
        )
        else -> LibraryScreen(library, loading, onOpen = ::openUri, onSettings = { showSettings = true })
    }

    error?.let { message ->
        AlertDialog(
            onDismissRequest = { error = null },
            confirmButton = { TextButton(onClick = { error = null }) { Text("OK") } },
            title = { Text("Could not open") },
            text = { Text(message) },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class, androidx.compose.foundation.ExperimentalFoundationApi::class)
@Composable
private fun LibraryScreen(library: Library, loading: Boolean, onOpen: (Uri) -> Unit, onSettings: () -> Unit) {
    val books = remember { mutableStateListOf<RecentBook>().apply { addAll(library.recent()) } }
    var forget by remember { mutableStateOf<RecentBook?>(null) }
    val snackbar = remember { SnackbarHostState() }
    val pick = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> uri?.let(onOpen) }
    LaunchedEffect(loading) {
        if (!loading) {
            books.clear()
            books.addAll(library.recent())
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("VNMF Reader") },
                actions = { IconButton(onClick = onSettings) { Icon(Icons.Filled.Settings, "Reading settings") } },
            )
        },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = { pick.launch(arrayOf("application/x-vnmf", "application/octet-stream", "*/*")) },
                icon = { Icon(Icons.Filled.Add, null) },
                text = { Text("Open file") },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            if (books.isEmpty()) {
                Text(
                    "Opened books will appear here.\nOpen a .vnmf file with the button below or from a file manager.",
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodyLarge,
                    modifier = Modifier.align(Alignment.Center).padding(32.dp),
                )
            } else {
                LazyVerticalGrid(
                    columns = GridCells.Adaptive(116.dp),
                    contentPadding = PaddingValues(12.dp, 12.dp, 12.dp, 96.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    items(books, key = { it.uuid }) { book ->
                        Column(Modifier.combinedClickable(onClick = { onOpen(book.uri) }, onLongClick = { forget = book })) {
                            // Cover from the file — in the background, not on the main thread while the list scrolls.
                            val cover by produceState<android.graphics.Bitmap?>(null, book.uuid) {
                                value = withContext(Dispatchers.IO) { library.cover(book.uuid) }
                            }
                            Box(
                                Modifier.fillMaxWidth().aspectRatio(0.7f).clip(RoundedCornerShape(8.dp))
                                    .background(MaterialTheme.colorScheme.surfaceVariant),
                            ) {
                                cover?.let { Image(it.asImageBitmap(), null, Modifier.fillMaxSize(), contentScale = ContentScale.Crop) }
                            }
                            Text(book.title, maxLines = 2, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(top = 6.dp))
                            library.position(book.uuid)?.let {
                                Text("p. ${it.page + 1} of ${book.pages}", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                        }
                    }
                }
            }
            if (loading) CircularProgressIndicator(Modifier.align(Alignment.Center))
        }
    }

    forget?.let { book ->
        AlertDialog(
            onDismissRequest = { forget = null },
            confirmButton = {
                TextButton(onClick = {
                    library.forget(book)
                    books.remove(book)
                    forget = null
                }) { Text("Remove") }
            },
            dismissButton = { TextButton(onClick = { forget = null }) { Text("Cancel") } },
            title = { Text("Remove from library?") },
            text = { Text("“${book.title}” will disappear from the list. The file itself stays where it is.") },
        )
    }
}
