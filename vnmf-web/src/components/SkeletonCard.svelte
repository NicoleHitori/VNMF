<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- The outline of a book card while books are loading. -->
<div class="skeleton" aria-hidden="true">
  <div class="cover shimmer"></div>
  <div class="line shimmer"></div>
  <div class="line short shimmer"></div>
</div>

<style>
  .skeleton {
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    min-width: 0;
  }
  .cover {
    aspect-ratio: 2 / 3;
    border-radius: 10px;
    border: 1px solid var(--border);
  }
  .line {
    height: 0.8rem;
    border-radius: 6px;
    width: 85%;
  }
  .short {
    width: 55%;
  }
</style>
