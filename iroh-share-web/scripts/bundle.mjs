// Assemble a self-contained static directory, dist/iroh-share/: index.html with
// style.css and main.js inlined, the icon, and the wasm-bindgen output. Serve it
// anywhere. The named folder means `sendme send dist/iroh-share` publishes the
// app as `iroh-share/`, next to other apps if sent from a shared parent.
// No dependencies: plain Node fs.
import { readFileSync, writeFileSync, rmSync, mkdirSync, cpSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const web = join(root, "web");
const dist = join(root, "dist");
const out = join(dist, "iroh-share");

// Function replacement so `$` in the CSS/JS isn't treated as a replacement
// pattern; throw if a marker is missing so a broken build never ships silently.
function inline(html, marker, replacement) {
  if (!html.includes(marker)) throw new Error(`bundle: marker not found: ${marker}`);
  return html.replace(marker, () => replacement);
}

let html = readFileSync(join(web, "index.html"), "utf8");
const css = readFileSync(join(web, "style.css"), "utf8");
const js = readFileSync(join(web, "main.js"), "utf8");
html = inline(html, '<link rel="stylesheet" href="./style.css" />', `<style>\n${css}</style>`);
html = inline(html, '<script src="./main.js" type="module"></script>', `<script type="module">\n${js}</script>`);

rmSync(dist, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
writeFileSync(join(out, "index.html"), html);
cpSync(join(root, "..", "assets", "icon.svg"), join(out, "icon.svg"));
cpSync(join(web, "wasm"), join(out, "wasm"), { recursive: true });
console.log("bundled → dist/iroh-share/");
