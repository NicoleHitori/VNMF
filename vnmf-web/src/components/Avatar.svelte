<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script>
  // A user's avatar: the picture, or the first letter of the name on the accent gradient (like AdwAvatar).
  import { displayName } from '../lib/format.js';

  let { user, size = 32 } = $props();
  let failed = $state(false);

  $effect(() => {
    user?.avatar;
    failed = false;
  });
</script>

<span class="avatar" style:--size="{size}px" aria-hidden="true">
  {#if user?.avatar && !failed}
    <img src={user.avatar} alt="" onerror={() => (failed = true)} />
  {:else}
    {[...(displayName(user) || '?')][0].toUpperCase()}
  {/if}
</span>

<style>
  .avatar {
    flex: none;
    width: var(--size);
    height: var(--size);
    border-radius: 50%;
    overflow: hidden;
    display: inline-grid;
    place-items: center;
    background: linear-gradient(135deg, #f0a3c7, #b1407a);
    color: #fff;
    font-weight: 800;
    font-size: calc(var(--size) * 0.45);
    line-height: 1;
    user-select: none;
  }
  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
</style>
