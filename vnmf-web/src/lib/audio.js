// SPDX-License-Identifier: GPL-3.0-or-later
// Page sound: background music that continues until it changes, and one-shot effects — with the
// parameters from the file (loop, volume, delay, duration). The rules of *what* plays where come
// from the server (/playback, /strip-sfx), the same as in the desktop and Android readers.

const FADE_MS = 600;

function clamp(v) {
  return Math.min(1, Math.max(0, v));
}

function fadeOut(audio, done) {
  const start = audio.volume;
  const t0 = performance.now();
  const step = (now) => {
    const k = Math.min(1, (now - t0) / FADE_MS);
    audio.volume = start * (1 - k);
    if (k < 1) requestAnimationFrame(step);
    else {
      audio.pause();
      done?.();
    }
  };
  requestAnimationFrame(step);
}

export class SoundPlayer {
  constructor() {
    this.enabled = false;
    this.volume = 1;
    this.music = null; // { audio, info, timers }
    this.effects = new Set();
  }

  setEnabled(on) {
    this.enabled = on;
    if (!on) this.stopAll();
  }

  setVolume(v) {
    this.volume = clamp(v);
    if (this.music) this.music.audio.volume = clamp(this.music.info.gain * this.volume);
  }

  /** The music that should be playing now (`info` from the API, or null for silence). */
  setMusic(info, url) {
    if (!this.enabled) return;
    if (this.music?.info.index === info?.index) return;
    this.stopMusic();
    if (!info) return;
    const audio = new Audio(url);
    audio.loop = info.looped;
    audio.volume = clamp(info.gain * this.volume);
    const music = { audio, info, timers: [] };
    music.timers.push(setTimeout(() => audio.play().catch(() => {}), info.delay_ms));
    if (info.duration_ms > 0) music.timers.push(setTimeout(() => fadeOut(audio), info.delay_ms + info.duration_ms));
    this.music = music;
  }

  stopMusic() {
    const m = this.music;
    if (!m) return;
    m.timers.forEach(clearTimeout);
    fadeOut(m.audio, () => m.audio.removeAttribute('src'));
    this.music = null;
  }

  playEffect(info, url) {
    if (!this.enabled) return;
    const audio = new Audio(url);
    audio.loop = info.looped;
    audio.volume = clamp(info.gain * this.volume);
    const fx = { audio, timers: [] };
    const end = () => {
      fx.timers.forEach(clearTimeout);
      audio.pause();
      this.effects.delete(fx);
    };
    audio.onended = end;
    fx.timers.push(setTimeout(() => audio.play().catch(end), info.delay_ms));
    // A looped effect without a duration would play forever: limit it to a minute.
    const duration = info.duration_ms > 0 ? info.duration_ms : info.looped ? 60000 : 0;
    if (duration) fx.timers.push(setTimeout(() => fadeOut(audio, end), info.delay_ms + duration));
    this.effects.add(fx);
  }

  stopEffects() {
    for (const fx of this.effects) {
      fx.timers.forEach(clearTimeout);
      fx.audio.pause();
    }
    this.effects.clear();
  }

  stopAll() {
    this.stopMusic();
    this.stopEffects();
  }
}
