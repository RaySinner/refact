// Full build script for Refact VS Code: extension
// Orchestrates: GUI build -> LSP binary build/place -> VSCode: extension packaging
//
// Prerequisites: Rust (cargo), Node.js (npm), vsce globally installed
// Usage from repo root: node scripts/build-vscode.mjs

import { spawnSync } from "child_process";
import { existsSync, readdirSync, copyFileSync, mkdirSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import * as fs from "fs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "..");
const GUI_DIR = join(ROOT, "refact-agent", "gui");
const ENGINE_DIR = join(ROOT, "refact-agent", "engine");
const VSCODE_DIR = join(ROOT, "plugins", "vscode");
const VSCODE_ASSETS = join(VSCODE_DIR, "assets");

const IS_WIN = process.platform === "win32";
const BINARY_SRC = join(ENGINE_DIR, "target", "release", "refact-lsp" + (IS_WIN ? ".exe" : ""));
const BINARY_DST = join(VSCODE_ASSETS, IS_WIN ? "refact-lsp.exe" : "refact-lsp");

function cmdExt(base) {
  return IS_WIN ? `${base}.cmd` : base;
}

function run(label, cwd, cmd, args = [], env = {}, useShell = false) {
  console.log(`\n[${label}] ${cmd} ${args.join(" ")}`);
  const result = spawnSync(cmd, args, {
    cwd,
    stdio: "inherit",
    env: { ...process.env, ...env },
    shell: useShell,
  });
  if (result.status !== 0) {
    console.error(`\nFAILED: ${label} (exit ${result.status})`);
    process.exit(result.status || 1);
  }
  console.log(`${label} done`);
}

// 1. Build GUI
run("1/6 GUI: npm ci", GUI_DIR, cmdExt("npm"), ["ci"], {}, true);
run("2/6 GUI: tsc", GUI_DIR, cmdExt("npx"), ["tsc", "--noEmit"], {
  NODE_OPTIONS: "--max-old-space-size=16384",
}, true);
run("3/6 GUI: vite build (browser)", GUI_DIR, cmdExt("npx"), ["vite", "build"], {
  NODE_OPTIONS: "--max-old-space-size=16384",
}, true);
run("4/6 GUI: vite build (node)", GUI_DIR, cmdExt("npx"), ["vite", "build", "-c", "vite.node.config.ts"], {
  NODE_OPTIONS: "--max-old-space-size=16384",
}, true);
run("5/6 GUI: npm pack", GUI_DIR, cmdExt("npm"), ["pack"], {}, true);

const tarballs = readdirSync(GUI_DIR).filter((f) => f.startsWith("refact-chat-js") && f.endsWith(".tgz"));
if (tarballs.length !== 1) {
  console.error(`Expected exactly one tarball, found: ${tarballs.join(", ")}`);
  process.exit(1);
}
const tarballPath = join(GUI_DIR, tarballs[0]);

// 2. Build / use LSP engine
if (!existsSync(BINARY_SRC)) {
  console.log("\n[6/6 Engine] Building Rust LSP engine (cold ~15-30 min)...");
  run("cargo build", ENGINE_DIR, "cargo", ["build", "--release"], {
    REFACT_SKIP_GUI_BUILD: "1",
  });
} else {
  console.log("\n[6/6 Engine] Using existing binary");
}

mkdirSync(VSCODE_ASSETS, { recursive: true });
copyFileSync(BINARY_SRC, BINARY_DST);
console.log(`Copied engine binary to ${BINARY_DST}`);

// 3. VSCode: extension
run("7/8 VSCode: npm ci", VSCODE_DIR, cmdExt("npm"), ["ci"], {}, true);
run("8/8 VSCode: install GUI", VSCODE_DIR, cmdExt("npm"), ["install", tarballPath, "--save-exact"], {}, true);
run("TypeScript compile", VSCODE_DIR, cmdExt("npm"), ["run", "compile"], {}, true);
run("vsce package", VSCODE_DIR, cmdExt("vsce"), ["package", "--target", "win32-x64"], {}, true);

// 4. Restore package.json
const pkgPath = join(VSCODE_DIR, "package.json");
const pkg = JSON.parse(fs.readFileSync(pkgPath, "utf8"));
delete pkg.dependencies["refact-chat-js"];
fs.writeFileSync(pkgPath, JSON.stringify(pkg, null, "\t") + "\n");
console.log("Restored package.json");

// 5. Report
const vsixFiles = readdirSync(VSCODE_DIR).filter((f) => f.endsWith(".vsix"));
console.log("\nBUILD COMPLETE!");
for (const f of vsixFiles) {
  console.log(`  ${join("plugins/vscode", f)}`);
}
process.exit(0);
