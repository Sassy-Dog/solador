/**
 * Rasterises `brand/icon.svg` into the icon set `scripts/generate-icons.sh`
 * assembles. Driven by that script; not meant to be run directly.
 *
 * Chromium rather than ImageMagick, for a specific reason. IM's configured
 * `rsvg-convert` delegate is absent on a stock macOS box, so it silently falls
 * back to MSVG -- its own incomplete SVG renderer -- which mishandles
 * `transform="rotate(angle cx cy)"`. `brand/icon.svg` is built from exactly
 * that, and MSVG renders the out-of-true tile in the wrong grid cell. The bug
 * is quiet: you get a plausible icon that is not the mark. Chromium is already
 * a dev dependency here (tests/frontend), and it is the same class of engine
 * the app's own WebView uses.
 *
 * Every size is rendered from the vector at its native size rather than
 * downscaled from one master, so the 16px icon is drawn at 16px instead of
 * being a resampled 1024.
 */
import { readFileSync, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';

const [, , svgPath, outDir, spec, nodeModules] = process.argv;
if (!svgPath || !outDir || !spec || !nodeModules) {
  console.error('usage: render-icons.mjs <icon.svg> <outDir> <name:size:shape,...> <node_modules>');
  process.exit(2);
}

// Resolved from the frontend suite's install rather than imported by bare
// specifier. ESM resolves a bare import from the *importing file's* directory,
// and this file lives in scripts/ where there is no node_modules -- so a plain
// `import 'playwright'` fails no matter what the caller's cwd is.
const { chromium } = createRequire(
  path.resolve(nodeModules, '..', 'package.json') // createRequire rejects a relative path
)('playwright');

/**
 * macOS expects the artwork inset inside a rounded rect rather than filling the
 * canvas; Windows `.ico` expects the full square. `brand/icon.svg` carries no
 * radius precisely so this step can decide, and so a radius is never applied
 * twice. 80.47% and 22.37% are the proportions Apple's own icon grid uses.
 */
const MACOS_INSET = 0.8047;
const MACOS_RADIUS = 22.37;

const svg = readFileSync(path.resolve(svgPath), 'utf8');
const dataUri = `data:image/svg+xml;base64,${Buffer.from(svg).toString('base64')}`;

mkdirSync(outDir, { recursive: true });

const browser = await chromium.launch();
const written = [];

for (const entry of spec.split(',')) {
  const [name, sizeRaw, shape] = entry.split(':');
  const size = Number(sizeRaw);
  const rounded = shape === 'rounded';
  const art = rounded ? Math.round(size * MACOS_INSET) : size;
  const offset = Math.round((size - art) / 2);

  // `viewport`, NOT `viewportSize`. The latter is a Page *getter*, not a
  // newPage option, so Playwright ignores it in silence and leaves the default
  // 1280x720 -- which clipped the 1024px representation to 1024x720, and
  // iconutil then dropped that slice from the .icns without complaint. Two
  // silent failures in a row produced an icon set missing its Retina master.
  const page = await browser.newPage({
    viewport: { width: size, height: size },
    deviceScaleFactor: 1,
  });
  await page.setContent(
    `<!doctype html><meta charset="utf-8">
     <style>
       html,body{margin:0;padding:0;background:transparent}
       .frame{width:${size}px;height:${size}px;position:relative}
       img{position:absolute;left:${offset}px;top:${offset}px;
           width:${art}px;height:${art}px;
           border-radius:${rounded ? `${MACOS_RADIUS}%` : '0'}}
     </style>
     <div class="frame"><img src="${dataUri}" alt=""></div>`
  );
  await page.screenshot({
    path: path.join(outDir, name),
    omitBackground: true,
    clip: { x: 0, y: 0, width: size, height: size },
  });
  await page.close();
  written.push(`${name} ${size}px${rounded ? ' rounded' : ''}`);
}

await browser.close();
console.log(written.join('\n'));
