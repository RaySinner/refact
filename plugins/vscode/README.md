# Refact VS Code Plugin

This VS Code extension is part of the [smallcloudai/refact](https://github.com/smallcloudai/refact) monorepo.

## Development

### Quick build (one command)

From the repo root:

```bash
node scripts/build-vscode.mjs
```

This script orchestrates the full pipeline: GUI build, LSP engine build/place, and VSCode: extension packaging.

### Manual steps

If you need to run each step individually:

1. Build GUI package:
   ```bash
   cd refact-agent/gui
   npm ci
   # On Windows use cmd so NODE_OPTIONS is parsed correctly:
   cmd /c "set NODE_OPTIONS=--max-old-space-size=16384 && npx tsc --noEmit && npx vite build && npx vite build -c vite.node.config.ts"
   npm pack
   ```

2. Build LSP engine:
   ```bash
   cd refact-agent/engine
   # Set REFACT_SKIP_GUI_BUILD=1 to skip embedded GUI (VSCode: loads it from node_modules)
   set REFACT_SKIP_GUI_BUILD=1 && cargo build --release
   ```

3. Package VSCode: extension:
   ```bash
   cd plugins/vscode
   npm ci
   npm install ../../refact-agent/gui/refact-chat-js-*.tgz --save-exact
   npm run compile
   vsce package --target win32-x64
   ```

The extension packages the local `refact-lsp` engine and `refact-chat-js` UI artifacts.

## Repository history

This code was migrated from the archived standalone VS Code plugin repository. Historical releases and tags remain available there for reference.

## Issues

Please report issues in the monorepo: <https://github.com/smallcloudai/refact/issues>.
