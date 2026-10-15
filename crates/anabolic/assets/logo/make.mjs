// The Anabolic mark, "Steroid Core": a processor whose die is the steroid
// skeleton (gonane: three fused hexagons and a pentagon), its hot ring
// burning like a busy core, traces running out to the pins.
//
// This script is the source of truth. It writes logo.svg (app icon, on its
// tile), mark.svg (the bare mark, for the navigation rail) and sheet.html,
// which lays every raster size out for rasterize.ps1 to turn into
// anabolic.ico and the raw RGBA images the app embeds. wordmark.py then sets
// the name beside the mark (wordmark.svg, lockup.svg).
//
//   node make.mjs && python wordmark.py && powershell -File rasterize.ps1

import { writeFileSync } from "node:fs";

const O = "#FF6A3D", O2 = "#FFA07A", OD = "#B33914", GOLD = "#E8B04A", GOLD2 = "#F8DA94";

function gonane(cx, cy, r) {
  const s3 = Math.sqrt(3);
  const hex = (x, y) => [...Array(6)].map((_, i) => {
    const a = (-90 + 60 * i) * Math.PI / 180;
    return [x + r * Math.cos(a), y + r * Math.sin(a)];
  });
  const A = [cx, cy], B = [cx + r * s3, cy], C = [B[0] + r * s3 / 2, B[1] - 1.5 * r];
  const ex = C[0] + r * s3 / 2, pc = [ex + 0.688 * r, C[1]], R = 0.8507 * r;
  const pent = [144, 216, 288, 0, 72].map(a => [pc[0] + R * Math.cos(a * Math.PI / 180), pc[1] + R * Math.sin(a * Math.PI / 180)]);
  const pts = p => p.map(q => q.map(v => +v.toFixed(2)).join(",")).join(" ");
  return { rings: [pts(hex(...A)), pts(hex(...B)), pts(hex(...C)), pts(pent)], hot: C, verts: [...hex(...A), ...hex(...B), ...hex(...C), ...pent] };
}
// The skeleton is 5.866 r wide and 3.5 r tall; centre it in a box `w` wide.
const fitted = (cx, cy, w) => { const r = w / 5.866; return gonane(cx - 2.067 * r, cy + 0.75 * r, r); };

/**
 * @param {object} o
 * @param {boolean} o.tile   draw the app icon's rounded square behind it
 * @param {boolean} o.small  the 16-24 px cut: fewer pins, heavier lines, no traces
 * @param {string}  o.id     unique suffix for gradient ids
 */
export function mark({ tile = true, small = false, id = "a" } = {}) {
  const pins = small ? 3 : 5;
  const x = small ? 13 : 12, w = small ? 38 : 40;
  const pinW = small ? 3.6 : 2.6, pinL = small ? 7 : 6.5;
  const g = fitted(32, 32, small ? 31 : 30);
  const step = w / (pins + 1);
  let s = `<defs>
    <linearGradient id="tile-${id}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#252b36"/><stop offset="1" stop-color="#0e1116"/></linearGradient>
    <linearGradient id="hot-${id}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${O2}"/><stop offset=".45" stop-color="${O}"/><stop offset="1" stop-color="${OD}"/></linearGradient>
    <linearGradient id="pin-${id}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${GOLD2}"/><stop offset="1" stop-color="${GOLD}"/></linearGradient>
    <linearGradient id="die-${id}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#3D4450"/><stop offset="1" stop-color="#181C22"/></linearGradient>
    <radialGradient id="glow-${id}" cx=".5" cy=".5" r=".5"><stop offset="0" stop-color="${O}" stop-opacity=".45"/><stop offset="1" stop-color="${O}" stop-opacity="0"/></radialGradient>
  </defs>`;
  if (tile) {
    s += `<rect x="2" y="2" width="60" height="60" rx="14" fill="url(#tile-${id})"/>`;
    s += `<rect x="2.5" y="2.5" width="59" height="59" rx="13.5" fill="none" stroke="#fff" stroke-opacity=".07"/>`;
  }
  for (let i = 1; i <= pins; i++) {
    const p = +(x + step * i - pinW / 2).toFixed(2);
    const q = +(x + step * i - pinW / 2).toFixed(2);
    s += `<rect x="${p}" y="${x - pinL}" width="${pinW}" height="${pinL}" rx="${pinW / 3}" fill="url(#pin-${id})"/>`;
    s += `<rect x="${p}" y="${x + w}" width="${pinW}" height="${pinL}" rx="${pinW / 3}" fill="url(#pin-${id})"/>`;
    s += `<rect x="${x - pinL}" y="${q}" width="${pinL}" height="${pinW}" rx="${pinW / 3}" fill="url(#pin-${id})"/>`;
    s += `<rect x="${x + w}" y="${q}" width="${pinL}" height="${pinW}" rx="${pinW / 3}" fill="url(#pin-${id})"/>`;
  }
  s += `<rect x="${x}" y="${x}" width="${w}" height="${w}" rx="4.5" fill="url(#die-${id})" stroke="#58626F" stroke-width="1"/>`;
  if (!small) {
    s += `<rect x="${x + 1.5}" y="${x + 1.5}" width="${w - 3}" height="${w - 3}" rx="3.5" fill="none" stroke="#fff" stroke-opacity=".07"/>`;
    const v = g.verts;
    const trace = (a, b) => `<path d="M${a[0].toFixed(2)} ${a[1].toFixed(2)} L${b[0].toFixed(2)} ${b[1].toFixed(2)}" stroke="${GOLD}" stroke-opacity=".5" stroke-width="1"/>`;
    s += trace(v[0], [v[0][0], x + 0.5]) + trace(v[3], [v[3][0], x + w - 0.5]) + trace(v[9], [v[9][0], x + w - 0.5])
      + trace(v[12], [v[12][0], x + 0.5]) + trace(v[21], [x + w - 0.5, v[21][1]]);
  }
  s += `<circle cx="${g.hot[0].toFixed(2)}" cy="${g.hot[1].toFixed(2)}" r="12" fill="url(#glow-${id})"/>`;
  s += g.rings.map((r, i) => `<polygon points="${r}" fill="${i === 2 ? `url(#hot-${id})` : "none"}" stroke="${GOLD}" stroke-width="${small ? 2.8 : 2.1}" stroke-linejoin="round"/>`).join("");
  return s;
}

const svg = (body, size = 64) => `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 64 64">${body}</svg>`;

writeFileSync(new URL("./logo.svg", import.meta.url), svg(mark({ tile: true })) + "\n");
writeFileSync(new URL("./mark.svg", import.meta.url), svg(mark({ tile: false })) + "\n");

// Every raster the app needs, side by side on a transparent sheet:
// [name, pixel size, tile, small cut].
export const sizes = [
  ["icon-256", 256, true, false], ["icon-128", 128, true, false], ["icon-96", 96, true, false],
  ["icon-64", 64, true, false], ["icon-48", 48, true, false], ["icon-40", 40, true, false],
  ["icon-32", 32, true, false], ["icon-24", 24, true, true], ["icon-20", 20, true, true],
  ["icon-16", 16, true, true], ["mark-64", 64, false, false],
];
let left = 0, cells = "", layout = [];
for (const [name, size, tile, small] of sizes) {
  cells += `<div style="position:absolute;left:${left}px;top:0">${svg(mark({ tile, small, id: name }), size)}</div>`;
  layout.push({ name, size, left });
  left += size + 8;
}
writeFileSync(new URL("./sheet.html", import.meta.url),
  `<!doctype html><html><head><meta charset="utf-8"><style>html,body{margin:0;background:transparent}</style></head><body>${cells}</body></html>\n`);
writeFileSync(new URL("./sheet.json", import.meta.url), JSON.stringify({ width: left, height: 256, layout }, null, 1) + "\n");
console.log(`sheet ${left}x256, ${layout.length} rasters`);
