// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

package org.vnmf.reader

import android.content.Context
import android.os.Handler
import android.os.Looper
import androidx.annotation.OptIn
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.ByteArrayDataSource
import androidx.media3.datasource.DataSource
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.ProgressiveMediaSource
import uniffi.vnmf_ffi.AuxInfo
import uniffi.vnmf_ffi.Book

/**
 * Page sound: music (one track, continues across pages until it changes) and effects (one-shot or
 * looped while visible). What should play is decided by Rust (`Book.musicAt`, `sfxOf`, `stripSfx`) —
 * the same rules as in the desktop reader. Sound plays straight from the file bytes, no temporary files.
 */
@OptIn(UnstableApi::class)
class Sound(private val context: Context, private val book: Book) {
    private val main = Handler(Looper.getMainLooper())
    private var music: Pair<UInt, ExoPlayer>? = null
    /** Music whose `duration_ms` has run out: its pages are silent until it changes (as on desktop). */
    private var expired: UInt? = null
    private val effects = mutableMapOf<UInt, ExoPlayer>()
    /** Generation of music and effect starts: a deferred start (delay) won't fire if something has changed meanwhile. */
    private var musicGen = 0
    private var sfxGen = 0

    var enabled = true
        set(value) {
            field = value
            if (!value) stopAll()
        }

    /** Master volume 0..1 ("Volume" setting): multiplied by each attachment's volume. */
    var masterVolume = 0.8f
        set(value) {
            if (field == value) return
            field = value
            music?.second?.volume = (musicGain * value)
        }
    private var musicGain = 1f

    /** Music for the current position: the same track keeps playing, a different one replaces it (with a fade-out). */
    fun setMusic(aux: AuxInfo?) {
        val want = aux?.takeIf { enabled }
        if (want != null && (want.index == music?.first || want.index == expired)) return
        expired = null
        music?.second?.let { fadeOut(it) }
        music = null
        val gen = ++musicGen
        want ?: return
        main.postDelayed({
            if (gen != musicGen) return@postDelayed
            val player = start(want, music = true)
            musicGain = want.gain.toFloat()
            music = want.index to player
            limit(want, { musicGen == gen }) {
                if (music?.first == want.index) {
                    expired = want.index
                    fadeOut(player)
                    music = null
                }
            }
        }, want.delayMs.toLong())
    }

    /** A one-shot (or looped) effect. */
    fun playEffect(aux: AuxInfo) {
        if (!enabled) return
        val gen = sfxGen
        main.postDelayed({
            if (gen != sfxGen || !enabled) return@postDelayed
            effects.remove(aux.index)?.release()
            val player = start(aux, music = false)
            effects[aux.index] = player
            player.addListener(object : Player.Listener {
                override fun onPlaybackStateChanged(state: Int) {
                    if (state == Player.STATE_ENDED && effects[aux.index] === player) {
                        effects.remove(aux.index)
                        player.release()
                    }
                }
            })
            limit(aux, { sfxGen == gen }) { effects.remove(aux.index)?.let { fadeOut(it) } }
        }, aux.delayMs.toLong())
    }

    /** A looped effect has left the screen — stop it; let a one-shot finish. */
    fun leaveEffect(aux: AuxInfo) {
        if (aux.looped) effects.remove(aux.index)?.let { fadeOut(it) }
    }

    /** New page in paged mode: the previous page's effects go silent. */
    fun clearEffects() {
        sfxGen++
        effects.values.forEach { fadeOut(it) }
        effects.clear()
    }

    fun stopAll() {
        musicGen++
        sfxGen++
        music?.second?.release()
        music = null
        effects.values.forEach { it.release() }
        effects.clear()
    }

    fun release() = stopAll()

    private fun start(aux: AuxInfo, music: Boolean): ExoPlayer {
        val bytes = book.auxData(aux.index)
        val factory = DataSource.Factory { ByteArrayDataSource(bytes) }
        val item = MediaItem.Builder().setUri("vnmf://aux/${aux.index}").setMimeType(aux.mime).build()
        val attributes = AudioAttributes.Builder()
            .setUsage(C.USAGE_MEDIA)
            .setContentType(if (music) C.AUDIO_CONTENT_TYPE_MUSIC else C.AUDIO_CONTENT_TYPE_SONIFICATION)
            .build()
        return ExoPlayer.Builder(context).build().apply {
            // Only music takes focus: an effect must not pause another app's player.
            setAudioAttributes(attributes, music)
            setMediaSource(ProgressiveMediaSource.Factory(factory).createMediaSource(item))
            repeatMode = if (aux.looped) Player.REPEAT_MODE_ONE else Player.REPEAT_MODE_OFF
            volume = aux.gain.toFloat() * masterVolume
            prepare()
            play()
        }
    }

    private fun limit(aux: AuxInfo, current: () -> Boolean, stop: () -> Unit) {
        if (aux.durationMs == 0u) return
        main.postDelayed({ if (current()) stop() }, aux.durationMs.toLong())
    }

    /** Fades out over 0.8 s and releases the player. */
    private fun fadeOut(player: ExoPlayer) {
        val start = player.volume
        val steps = 20
        for (i in 1..steps) {
            main.postDelayed({
                if (i == steps) player.release() else player.volume = start * (steps - i) / steps
            }, i * 40L)
        }
    }
}
