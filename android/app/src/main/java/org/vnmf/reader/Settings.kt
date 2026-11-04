// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

package org.vnmf.reader

import android.content.Context
import android.content.SharedPreferences
import androidx.annotation.StringRes
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
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlin.math.roundToInt
import kotlin.properties.ReadWriteProperty
import kotlin.reflect.KProperty

/** Reading area background — as on desktop. */
enum class Background(@StringRes val label: Int) {
    Theme(R.string.background_theme),
    Black(R.string.background_black),
    Gray(R.string.background_gray),
    White(R.string.background_white);

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
enum class Fit(@StringRes val label: Int) {
    /** The page (spread) entirely on screen. */
    Page(R.string.fit_page),
    /** Full width, then scroll down. */
    Width(R.string.fit_width),
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
                title = { Text(stringResource(R.string.reading_settings)) },
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back)) } },
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            item { Header(R.string.settings_view) }
            item {
                Choice(R.string.settings_background, R.string.settings_background_summary, Background.entries, settings.background, { stringResource(it.label) }) { settings.background = it }
            }
            item {
                Choice(R.string.settings_page, R.string.settings_page_summary, Fit.entries, settings.fit, { stringResource(it.label) }) { settings.fit = it }
            }
            item { Toggle(R.string.settings_tap_zones, R.string.settings_tap_zones_summary, settings.tapZones) { settings.tapZones = it } }
            item { Toggle(R.string.settings_auto_hide, R.string.settings_auto_hide_summary, settings.autoHide) { settings.autoHide = it } }
            item { Toggle(R.string.settings_immersive, R.string.settings_immersive_summary, settings.immersive) { settings.immersive = it } }

            item { Header(R.string.settings_manga, R.string.settings_manga_summary) }
            item { Toggle(R.string.settings_spread, R.string.settings_spread_summary, settings.doublePage) { settings.doublePage = it } }
            item { Toggle(R.string.settings_cover_alone, R.string.settings_cover_alone_summary, settings.coverAlone) { settings.coverAlone = it } }
            item { Number(R.string.settings_preload, R.string.settings_preload_summary, settings.preload, 0..6) { settings.preload = it } }

            item { Header(R.string.settings_webtoon, R.string.settings_webtoon_summary) }
            item { Number(R.string.settings_strip_width, R.string.settings_strip_width_summary, settings.stripWidth, 30..100, step = 5) { settings.stripWidth = it } }
            item { Number(R.string.settings_strip_gap, null, settings.stripGap, 0..32) { settings.stripGap = it } }

            item { Header(R.string.settings_attachments, R.string.settings_attachments_summary) }
            item { Toggle(R.string.sound, R.string.settings_sound_summary, settings.sound) { settings.sound = it } }
            item { Number(R.string.settings_volume, R.string.settings_volume_summary, settings.volume, 0..100, step = 5, enabled = settings.sound) { settings.volume = it } }
            item { Toggle(R.string.settings_dark_pages, R.string.settings_dark_pages_summary, settings.darkPages) { settings.darkPages = it } }

            item { Header(R.string.settings_position) }
            item { Toggle(R.string.settings_remember, R.string.settings_remember_summary, settings.rememberPosition) { settings.rememberPosition = it } }
            item {
                var done by remember { mutableStateOf(false) }
                OutlinedButton(
                    onClick = {
                        library.forgetPositions()
                        done = true
                    },
                    enabled = !done,
                    modifier = Modifier.fillMaxWidth().padding(16.dp, 8.dp, 16.dp, 24.dp),
                ) { Text(stringResource(if (done) R.string.settings_forgotten else R.string.settings_forget), textAlign = TextAlign.Center) }
            }
        }
    }
}

@Composable
private fun Header(@StringRes title: Int, @StringRes description: Int? = null) {
    Column(Modifier.padding(16.dp, 20.dp, 16.dp, 4.dp)) {
        Text(stringResource(title), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
        description?.let { Text(stringResource(it), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
    }
}

@Composable
private fun Toggle(@StringRes title: Int, @StringRes subtitle: Int, value: Boolean, onChange: (Boolean) -> Unit) {
    ListItem(
        headlineContent = { Text(stringResource(title)) },
        supportingContent = { Text(stringResource(subtitle)) },
        trailingContent = { Switch(checked = value, onCheckedChange = onChange) },
        modifier = Modifier.padding(0.dp),
    )
}

@Composable
private fun Number(@StringRes title: Int, @StringRes subtitle: Int?, value: Int, range: IntRange, step: Int = 1, enabled: Boolean = true, onChange: (Int) -> Unit) {
    ListItem(
        headlineContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(stringResource(title), Modifier.weight(1f))
                Text(value.toString(), style = MaterialTheme.typography.labelLarge)
            }
        },
        supportingContent = {
            Column {
                subtitle?.let { Text(stringResource(it)) }
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
private fun <T> Choice(@StringRes title: Int, @StringRes subtitle: Int, options: List<T>, value: T, label: @Composable (T) -> String, onChange: (T) -> Unit) {
    ListItem(
        headlineContent = { Text(stringResource(title)) },
        supportingContent = {
            Column {
                Text(stringResource(subtitle))
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
