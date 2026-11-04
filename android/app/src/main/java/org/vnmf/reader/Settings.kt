// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

package org.vnmf.reader

import android.content.Context
import android.content.SharedPreferences
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlin.math.roundToInt
import kotlin.properties.ReadWriteProperty
import kotlin.reflect.KProperty

/** Reading area background — as on desktop. */
enum class Background(val label: String) {
    Theme("Follow theme"),
    Black("Black"),
    Gray("Gray"),
    White("White");

    /** `null` — the theme color. */
    val color: Color?
        get() = when (this) {
            Theme -> null
            Black -> Color.Black
            Gray -> Color(0xFF3A3A3A)
            White -> Color.White
        }
}

/** How to fit a manga page. */
enum class Fit(val label: String) {
    /** The page (spread) entirely on screen. */
    Page("Whole page"),
    /** Full width, then scroll down. */
    Width("Fit width"),
}

/**
 * Reading settings — the same as in the desktop reader (`reader.json`), adapted for a phone:
 * strip width is in percent of the screen, spreads are in landscape orientation. Each value is
 * Compose state: a change is visible on screen immediately and is saved.
 */
class ReaderSettings(context: Context) {
    private val prefs: SharedPreferences = context.getSharedPreferences("reader", Context.MODE_PRIVATE)

    // View
    var background by enumPref("background", Background.Theme)
    var fit by enumPref("fit", Fit.Page)
    /** Tapping the screen edges turns pages (the middle toggles the bar). */
    var tapZones by boolPref("tap_zones", true)
    /** The reader bar disappears by itself after a few seconds. */
    var autoHide by boolPref("auto_hide", true)
    /** System bars (clock, navigation) are hidden while reading. */
    var immersive by boolPref("immersive", true)

    // Manga
    /** Two pages side by side (in landscape orientation). */
    var doublePage by boolPref("double_page", true)
    var coverAlone by boolPref("cover_alone", true)
    /** How many pages ahead to prepare. */
    var preload by intPref("preload", 2)

    // Webtoon
    /** Strip width, % of the screen width. */
    var stripWidth by intPref("strip_width", 100)
    /** Gap between fragments, dp. */
    var stripGap by intPref("strip_gap", 0)

    // Sound and attachments
    var sound by boolPref("sound", true)
    var volume by intPref("volume", 80)
    var darkPages by boolPref("dark_pages", true)

    // Position
    var rememberPosition by boolPref("remember_position", true)

    private fun boolPref(key: String, default: Boolean) = Pref(prefs.getBoolean(key, default)) { prefs.edit().putBoolean(key, it).apply() }
    private fun intPref(key: String, default: Int) = Pref(prefs.getInt(key, default)) { prefs.edit().putInt(key, it).apply() }
    private inline fun <reified E : Enum<E>> enumPref(key: String, default: E): Pref<E> {
        val stored = prefs.getString(key, null)?.let { name -> enumValues<E>().firstOrNull { it.name == name } }
        return Pref(stored ?: default) { prefs.edit().putString(key, it.name).apply() }
    }

    private class Pref<T>(initial: T, private val save: (T) -> Unit) : ReadWriteProperty<Any?, T> {
        private val state: MutableState<T> = mutableStateOf(initial)
        override fun getValue(thisRef: Any?, property: KProperty<*>): T = state.value
        override fun setValue(thisRef: Any?, property: KProperty<*>, value: T) {
            state.value = value
            save(value)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(settings: ReaderSettings, library: Library, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Reading settings") },
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            item { Header("View") }
            item {
                Choice("Background", "Color around the page", Background.entries, settings.background, { it.label }) { settings.background = it }
            }
            item {
                Choice("Page", "Entirely on screen or full width with scrolling", Fit.entries, settings.fit, { it.label }) { settings.fit = it }
            }
            item { Toggle("Turn pages by tapping the edges", "The left and right screen edges turn pages, the middle toggles the bar", settings.tapZones) { settings.tapZones = it } }
            item { Toggle("Hide the reader bar", "Disappears by itself after a few seconds", settings.autoHide) { settings.autoHide = it } }
            item { Toggle("Fullscreen reading", "System bars are hidden; they appear together with the reader bar", settings.immersive) { settings.immersive = it } }

            item { Header("Manga", "Paged mode") }
            item { Toggle("Spread", "In landscape — two pages side by side; wide pages shown alone", settings.doublePage) { settings.doublePage = it } }
            item { Toggle("Cover alone", "In a spread the first page is shown by itself", settings.coverAlone) { settings.coverAlone = it } }
            item { Number("Preloading", "How many pages ahead to prepare", settings.preload, 0..6) { settings.preload = it } }

            item { Header("Webtoon", "Strip mode") }
            item { Number("Strip width, %", "100% — full screen width", settings.stripWidth, 30..100, step = 5) { settings.stripWidth = it } }
            item { Number("Gap between fragments, dp", null, settings.stripGap, 0..32) { settings.stripGap = it } }

            item { Header("Sound and page attachments", "If the file contains them: music, sound effects, text and dark versions of pages") }
            item { Toggle("Sound", "Page music and effects", settings.sound) { settings.sound = it } }
            item { Number("Volume, %", "Shared by music and effects", settings.volume, 0..100, step = 5, enabled = settings.sound) { settings.volume = it } }
            item { Toggle("Dark page versions", "Used instead of the regular ones in a dark theme", settings.darkPages) { settings.darkPages = it } }

            item { Header("Reading position") }
            item { Toggle("Remember the page", "Open each book where you left off (by file UUID)", settings.rememberPosition) { settings.rememberPosition = it } }
            item {
                var done by remember { mutableStateOf(false) }
                OutlinedButton(
                    onClick = {
                        library.forgetPositions()
                        done = true
                    },
                    enabled = !done,
                    modifier = Modifier.fillMaxWidth().padding(16.dp, 8.dp, 16.dp, 24.dp),
                ) { Text(if (done) "Positions forgotten" else "Forget all saved positions", textAlign = TextAlign.Center) }
            }
        }
    }
}

@Composable
private fun Header(title: String, description: String? = null) {
    Column(Modifier.padding(16.dp, 20.dp, 16.dp, 4.dp)) {
        Text(title, style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
        description?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
    }
}

@Composable
private fun Toggle(title: String, subtitle: String, value: Boolean, onChange: (Boolean) -> Unit) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = { Text(subtitle) },
        trailingContent = { Switch(checked = value, onCheckedChange = onChange) },
        modifier = Modifier.padding(0.dp),
    )
}

@Composable
private fun Number(title: String, subtitle: String?, value: Int, range: IntRange, step: Int = 1, enabled: Boolean = true, onChange: (Int) -> Unit) {
    ListItem(
        headlineContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(title, Modifier.weight(1f))
                Text(value.toString(), style = MaterialTheme.typography.labelLarge)
            }
        },
        supportingContent = {
            Column {
                subtitle?.let { Text(it) }
                Slider(
                    value = value.toFloat(),
                    onValueChange = { onChange((it / step).roundToInt() * step) },
                    valueRange = range.first.toFloat()..range.last.toFloat(),
                    steps = ((range.last - range.first) / step - 1).coerceAtLeast(0),
                    enabled = enabled,
                )
            }
        },
    )
}

@Composable
private fun <T> Choice(title: String, subtitle: String, options: List<T>, value: T, label: (T) -> String, onChange: (T) -> Unit) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = {
            Column {
                Text(subtitle)
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(top = 8.dp)) {
                    options.forEachIndexed { i, option ->
                        SegmentedButton(
                            selected = option == value,
                            onClick = { onChange(option) },
                            shape = SegmentedButtonDefaults.itemShape(i, options.size),
                        ) { Text(label(option), maxLines = 1) }
                    }
                }
            }
        },
    )
}
