/* Browser-only preparation; one source and one canvas at a time. No dependencies. */
(function (root) {
  'use strict';
  const edge = 1536, maxJpeg = 2 * 1024 * 1024;
  const aborted = () => { const e = Error('Подготовка отменена'); e.name = 'AbortError'; throw e; };
  function dimensions(width, height) {
    if (!Number.isInteger(width) || !Number.isInteger(height) || width < 1 || height < 1) throw Error('Неверные размеры изображения');
    const scale = Math.min(1, edge / Math.max(width, height));
    const drawWidth = Math.max(1, Math.round(width * scale)), drawHeight = Math.max(1, Math.round(height * scale));
    return {width: Math.ceil(drawWidth / 8) * 8, height: Math.ceil(drawHeight / 8) * 8, drawWidth, drawHeight};
  }
  function outputName(name) { return name.replace(/\.[^.]*$/, '') + '.frame.jpg'; }
  function baseline(bytes) {
    if (bytes[0] !== 255 || bytes[1] !== 216) return false;
    let at = 2;
    while (at + 4 <= bytes.length) {
      if (bytes[at] !== 255) return false;
      const marker = bytes[at + 1], len = (bytes[at + 2] << 8) | bytes[at + 3];
      if (len < 2 || at + 2 + len > bytes.length) return false;
      if (marker === 192) return bytes[at + 4] === 8 && ((bytes[at + 5] << 8) | bytes[at + 6]) % 8 === 0 && ((bytes[at + 7] << 8) | bytes[at + 8]) % 8 === 0;
      if ([193, 194, 195, 197, 198, 199, 201, 202, 203, 205, 206, 207].includes(marker)) return false;
      at += 2 + len;
    }
    return false;
  }
  async function decode(file) {
    if (typeof createImageBitmap === 'function') return createImageBitmap(file, {imageOrientation: 'from-image'});
    const url = URL.createObjectURL(file), image = new Image();
    try { await new Promise((resolve, reject) => { image.onload = resolve; image.onerror = () => reject(Error('Браузер не смог открыть изображение')); image.src = url; }); }
    finally { URL.revokeObjectURL(url); }
    return image;
  }
  async function prepare(file, options = {}) {
    const check = () => { if (options.cancelled?.()) aborted(); };
    check();
    const image = await decode(file), canvas = document.createElement('canvas');
    try {
      check();
      const d = dimensions(image.width || image.naturalWidth, image.height || image.naturalHeight);
      canvas.width = d.width; canvas.height = d.height;
      const context = canvas.getContext('2d');
      if (!context) throw Error('Canvas недоступен');
      context.fillStyle = '#000'; context.fillRect(0, 0, d.width, d.height);
      context.drawImage(image, Math.floor((d.width - d.drawWidth) / 2), Math.floor((d.height - d.drawHeight) / 2), d.drawWidth, d.drawHeight);
      await new Promise(resolve => setTimeout(resolve, 0)); check();
      let blob;
      for (const quality of [.90, .85, .80, .75]) {
        check();
        blob = await new Promise((resolve, reject) => canvas.toBlob(b => b ? resolve(b) : reject(Error('Не удалось создать JPEG')), 'image/jpeg', quality));
        if (blob.size <= maxJpeg) break;
      }
      if (blob.size > maxJpeg || !baseline(new Uint8Array(await blob.arrayBuffer()))) throw Error('Браузер не создал подходящий baseline JPEG');
      check();
      return new File([blob], outputName(file.name), {type: blob.type, lastModified: file.lastModified});
    } finally { image.close?.(); canvas.width = canvas.height = 0; }
  }
  root.P4PhotoTools = {prepare, dimensions, outputName, baseline};
})(typeof window !== 'undefined' ? window : globalThis);
