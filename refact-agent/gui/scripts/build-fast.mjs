// Fast local GUI build: same bundles as `npm run build`, minus the
// vite-plugin-eslint pass over 1500+ files. The dts (rollupTypes) plugin is
// kept on by default because plugins/vscode imports `refact-chat-js/dist/events`
// and TypeScript needs those declarations to compile.
//
// Set REFACT_SKIP_GUI_DTS=1 only if you are not packaging the tgz (pure engine
// rebuilds that embed the JS bundles and ignore the .d.ts files).
//
// Never use this for publishing a release -- `npm run build` stays the
// canonical command and is unchanged.
//
// Usage: npm run build:fast

import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const GUI_DIR = join(__dirname, "..");

const env = {
  ...process.env,
  REFACT_SKIP_GUI_LINT: "1",
  NODE_OPTIONS: "--max-old-space-size=8192",
};
if (process.env.REFACT_SKIP_GUI_DTS === "1") {
  env.REFACT_SKIP_GUI_DTS = "1";
}

const steps = [
  ["tsc", ["./node_modules/typescript/bin/tsc"]],
  ["vite build", ["./node_modules/vite/bin/vite.js", "build"]],
  [
    "vite build (node)",
    ["./node_modules/vite/bin/vite.js", "build", "-c", "vite.node.config.ts"],
  ],
];

for (const [label, args] of steps) {
  console.log(`\n[gui:fast] ${label}`);
  const result = spawnSync(process.execPath, args, {
    cwd: GUI_DIR,
    stdio: "inherit",
    env,
  });
  if (result.status !== 0) {
    console.error(`\nFAILED: ${label} (exit ${result.status})`);
    process.exit(result.status || 1);
  }
}

console.log("\n[gui:fast] done");
