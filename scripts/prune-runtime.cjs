// Prunes src-tauri/resources/dsh-runtime/node_modules for bundling:
// - dev-only files (.map, .d.ts, markdown, licenses, configs)
// - docs/test/example directories
// - native binaries for platforms other than win32-x64
// Runtime behavior is unchanged: Node only loads .js/.mjs/.cjs at runtime.

const fs = require('node:fs');
const path = require('node:path');

const ROOT = path.join(__dirname, '..', 'src-tauri', 'resources', 'dsh-runtime', 'node_modules');

const FILE_PATTERNS = [
  /\.map$/,
  /\.d\.[cm]?ts$/,
  /\.(md|markdown)$/i,
  /^(CHANGELOG|CHANGES|HISTORY|AUTHORS|NOTICE)(\.|$)/i,
  /^(tsconfig\..*|\.npmignore|\.eslintrc.*|\.editorconfig|\.gitattributes|\.gitignore)$/,
  /\.tsbuildinfo$/,
  // LICENSE files are KEPT (compliance); .txt files are KEPT (some packages
  // read runtime data from .txt — deleting them is a landmine for zero gain).
];

// Directories safe to drop. NOTE: no 'doc'/'docs' — the `yaml` npm package
// ships RUNTIME code in dist/doc/ (composer.js requires ../doc/directives.js);
// deleting it breaks dsh at load. Same caution applies to 'test' at dist level.
const DIR_NAMES = new Set([
  '__tests__', 'example', 'examples', '.github', 'coverage', 'benchmark', 'benchmarks',
]);

// Platform-specific native payloads: keep win32-x64 only.
const PLATFORM_PRUNE = [
  { dir: 'node-pty/prebuilds', keep: 'win32-x64' },
  { dir: 'koffi/build/koffi', keep: 'win32_x64' },
];

const DROP_PACKAGES = ['@img/sharp-wasm32']; // wasm sharp is never used on desktop win

let removedFiles = 0;
let removedBytes = 0;

function rm(target) {
  const stat = fs.statSync(target, { throwIfNoEntry: false });
  if (!stat) return;
  removedFiles++;
  removedBytes += stat.isDirectory() ? 0 : stat.size;
  fs.rmSync(target, { recursive: true, force: true });
}

function walk(dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (DIR_NAMES.has(entry.name.toLowerCase())) rm(full);
      else walk(full);
    } else if (FILE_PATTERNS.some((re) => re.test(entry.name))) {
      rm(full);
    }
  }
}

walk(ROOT);
for (const { dir, keep } of PLATFORM_PRUNE) {
  const base = path.join(ROOT, dir);
  if (!fs.existsSync(base)) continue;
  for (const entry of fs.readdirSync(base)) {
    if (entry !== keep) rm(path.join(base, entry));
  }
}
for (const pkg of DROP_PACKAGES) rm(path.join(ROOT, pkg));

console.log(`pruned ${removedFiles} entries, ~${(removedBytes / 1e6).toFixed(0)}MB freed (file bytes only)`);
