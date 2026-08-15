// Generates src-tauri/icons/{32x32.png,128x128.png,icon.png,icon.ico} from the
// DeepSeek whale logo (assets/deepseek-logo.svg, copied from the harness web
// app's favicon). The source SVG is monochrome with a dark-mode <style>; we
// recolor the path to brand blue (#4d6bfe) so the whale reads on any taskbar.

const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');
const { Resvg } = require('@resvg/resvg-js');

const SRC = path.join(__dirname, '..', 'assets', 'deepseek-logo.svg');
const OUT = path.join(__dirname, '..', 'src-tauri', 'icons');

function render(size) {
  const svg = fs
    .readFileSync(SRC, 'utf8')
    .replace(/(<path\b[^>]*?)\s*fill="[^"]*"/, '$1') // drop the stock fill…
    .replace('<path ', '<path fill="#4d6bfe" ');      // …then brand-blue it
  const resvg = new Resvg(svg, { fitTo: { mode: 'width', value: size } });
  return resvg.render().asPng();
}

// --- ICO writer (PNG-compressed entries, Windows Vista+) ---
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function ico(entries) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(entries.length, 4);
  let offset = 6 + entries.length * 16;
  const dirs = entries.map(({ png, size }) => {
    const e = Buffer.alloc(16);
    e[0] = size === 256 ? 0 : size;
    e[1] = size === 256 ? 0 : size;
    e.writeUInt16LE(1, 4);
    e.writeUInt16LE(32, 6);
    e.writeUInt32LE(png.length, 8);
    e.writeUInt32LE(offset, 12);
    offset += png.length;
    return e;
  });
  return Buffer.concat([header, ...dirs, ...entries.map((e) => e.png)]);
}

fs.mkdirSync(OUT, { recursive: true });
const png32 = render(32);
const png128 = render(128);
const png256 = render(256);
fs.writeFileSync(path.join(OUT, '32x32.png'), png32);
fs.writeFileSync(path.join(OUT, '128x128.png'), png128);
fs.writeFileSync(path.join(OUT, 'icon.png'), png256);
fs.writeFileSync(
  path.join(OUT, 'icon.ico'),
  ico([
    { png: png32, size: 32 },
    { png: png256, size: 256 },
  ]),
);
console.log('DeepSeek whale icons written to', OUT);
