# Contributing to the Refact VS Code Plugin

The VS Code plugin now lives in the Refact monorepo under `plugins/vscode`.

## Setup

### Quick build (one command)

```bash
node scripts/build-vscode.mjs
```

### Manual steps

```bash
# 1. Build GUI
cd refact-agent/gui
npm ci
# On Windows use cmd so NODE_OPTIONS is parsed correctly:
cmd /c "set NODE_OPTIONS=--max-old-space-size=16384 && npx tsc --noEmit && npx vite build && npx vite build -c vite.node.config.ts"
npm pack

# 2. Build engine
cd ../engine
set REFACT_SKIP_GUI_BUILD=1 && cargo build --release

# 3. Package extension
cd ../../plugins/vscode
npm ci
npm install ../../refact-agent/gui/refact-chat-js-*.tgz --save-exact
npm run compile
vsce package --target win32-x64
```

For local packaging, the engine binary needs to be at `plugins/vscode/assets/refact-lsp`.

## Issues

Report plugin issues in the monorepo issue tracker: <https://github.com/smallcloudai/refact/issues>.
