// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * A square avatar from any picture: the centre cut out and scaled to `size`×`size`.
 * WebP where the browser can encode it, otherwise PNG.
 */
export async function squareAvatar(file, size = 256) {
  const bitmap = await createImageBitmap(file);
  const side = Math.min(bitmap.width, bitmap.height);
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = size;
  const ctx = canvas.getContext('2d');
  ctx.imageSmoothingQuality = 'high';
  ctx.drawImage(bitmap, (bitmap.width - side) / 2, (bitmap.height - side) / 2, side, side, 0, 0, size, size);
  bitmap.close?.();
  const encode = (type, quality) => new Promise((resolve) => canvas.toBlob(resolve, type, quality));
  const webp = await encode('image/webp', 0.9);
  // Browsers that can't encode WebP silently return PNG.
  return webp?.type === 'image/webp' ? webp : encode('image/png');
}
