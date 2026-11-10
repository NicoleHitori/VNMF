<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // Symbolic icons in the spirit of the GNOME icon set: 16×16, single colour (currentColor).
  let { name, size = 16, label = undefined } = $props();

  const ICONS = {
    back: 'M10 3 5 8l5 5',
    forward: 'm6 3 5 5-5 5',
    close: 'm4 4 8 8M12 4l-8 8',
    menu: 'M2.5 4h11M2.5 8h11M2.5 12h11',
    contents: 'M2.5 3.5h1M6 3.5h7.5M2.5 8h1M6 8h7.5M2.5 12.5h1M6 12.5h7.5',
    bookmark: 'M4 2h8v12l-4-3-4 3z',
    text: 'M3 3.5h10M8 3.5V13M5.5 13h5',
    dark: 'M13 9.5A5.5 5.5 0 1 1 6.5 3a4.5 4.5 0 0 0 6.5 6.5z',
    sound: 'M2.5 6h2.5L8.5 3v10L5 10H2.5zM11 5.5a3.5 3.5 0 0 1 0 5M12.8 3.5a6 6 0 0 1 0 9',
    muted: 'M2.5 6h2.5L8.5 3v10L5 10H2.5zM11 6l3 4M14 6l-3 4',
    settings: 'M8 5.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5zM8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4',
    fullscreen: 'M2.5 6V2.5H6M10 2.5h3.5V6M13.5 10v3.5H10M6 13.5H2.5V10',
    search: 'M7 2.5a4.5 4.5 0 1 0 0 9 4.5 4.5 0 0 0 0-9zM10.3 10.3 13.5 13.5',
    filter: 'M2 3.5h12l-4.5 5.5v4l-3 1.5V9z',
    user: 'M8 2.5a2.75 2.75 0 1 0 0 5.5 2.75 2.75 0 0 0 0-5.5zM2.5 14a5.5 5.5 0 0 1 11 0',
    upload: 'M8 11V2.5M4.5 6 8 2.5 11.5 6M2.5 10.5v3h11v-3',
    download: 'M8 2.5V11M4.5 7.5 8 11l3.5-3.5M2.5 10.5v3h11v-3',
    library: 'M2.5 2.5h3v11h-3zM6.5 2.5h3v11h-3zM10.5 3.2l2.6-.7 2.4 10.4-2.6.7z',
    admin: 'M8 1.5 13.5 4v4c0 3.2-2.3 5.6-5.5 6.5C4.8 13.6 2.5 11.2 2.5 8V4z',
    language: 'M8 1.5a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13zM1.5 8h13M8 1.5c-2 2.2-2.8 4.4-2.8 6.5s.8 4.3 2.8 6.5M8 1.5c2 2.2 2.8 4.4 2.8 6.5s-.8 4.3-2.8 6.5',
    check: 'm3 8.5 3 3 7-7',
    trash: 'M3 4.5h10M6.5 4.5v-2h3v2M4.5 4.5l.7 9h5.6l.7-9',
    edit: 'M10.5 2.5l3 3-8 8h-3v-3z',
    image: 'M2.5 3h11v10h-11zM2.5 11l3.5-3.5 3 3 2-2 2.5 2.5M10.5 6a.5.5 0 1 0 0 .01',
    images: 'M4.5 4.5h9v9h-9zM4.5 11.5l2.5-2.5 2 2 1.5-1.5 3 3M2.5 11.5v-9h9',
    camera: 'M2 5h3l1.5-2h3L11 5h3v8H2zM8 6.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z',
    shield: 'M8 1.5 13.5 4v4c0 3.2-2.3 5.6-5.5 6.5C4.8 13.6 2.5 11.2 2.5 8V4zM5.5 8l2 2 3-3.5',
    refresh: 'M13 8a5 5 0 1 1-1.5-3.6M13 2.5v3h-3',
    play: 'M5 3v10l8-5z',
    plus: 'M8 3v10M3 8h10',
    computer: 'M2 3h12v8H2zM6 14h4M8 11v3',
    phone: 'M5 1.5h6v13H5zM7.3 12.5h1.4',
    warning: 'M8 2 14.5 13.5h-13zM8 6.5v3.5M8 11.8v.2',
  };
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 16 16"
  fill="none"
  stroke="currentColor"
  stroke-width="1.5"
  stroke-linecap="round"
  stroke-linejoin="round"
  role={label ? 'img' : undefined}
  aria-label={label}
  aria-hidden={label ? undefined : 'true'}
>
  <path d={ICONS[name] ?? ''} />
</svg>

<style>
  .icon {
    flex: none;
    display: inline-block;
    vertical-align: middle;
  }
</style>
