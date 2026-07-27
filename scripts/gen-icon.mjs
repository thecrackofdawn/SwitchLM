// Render the vector icon source (app-icon-scaled.svg) to a 1024px PNG, the input `tauri icon` expects.
// Usage: node scripts/gen-icon.mjs && npx tauri icon app-icon.png
// NOTE: the window/tray icon is embedded into the Rust binary at compile time by
// generate_context! (it fs::reads the icon — icon files are NOT a cargo rebuild trigger).
// So after regenerating icons, force the crate to recompile so the new icon re-embeds:
//   cargo clean -p switchlm --manifest-path src-tauri/Cargo.toml   (or just `touch src-tauri/src/lib.rs`)
// Windows also caches taskbar/tray glyphs — clear with `ie4uinit.exe -show` if stale.
import { Resvg } from "@resvg/resvg-js";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");

const svg = readFileSync(resolve(root, "app-icon-scaled.svg"));
const resvg = new Resvg(svg, {
  fitTo: { mode: "width", value: 1024 },
  background: "transparent",
});
const png = resvg.render().asPng();
writeFileSync(resolve(root, "app-icon.png"), png);
console.log("wrote app-icon.png (1024x1024) from app-icon-scaled.svg");
