import type { Capabilities, Config } from "../features/Config/configSlice";

const WEB_CAPABILITIES: Capabilities = {
  filesPanel: true,
  gitPanel: true,
  terminalPanel: true,
  openFileInApp: true,
  openFileInIde: false,
  ideDiffPasteBack: false,
  folderPicker: true,
};

const IDE_CAPABILITIES: Capabilities = {
  filesPanel: false,
  gitPanel: false,
  terminalPanel: false,
  openFileInApp: false,
  openFileInIde: true,
  ideDiffPasteBack: true,
  folderPicker: false,
};

const VSCODE_CAPABILITIES: Capabilities = {
  ...IDE_CAPABILITIES,
  terminalPanel: true,
};

export function resolveCapabilities(
  host: Config["host"],
  overrides?: Partial<Capabilities>,
): Capabilities {
  const defaults =
    host === "web"
      ? WEB_CAPABILITIES
      : host === "vscode"
        ? VSCODE_CAPABILITIES
        : IDE_CAPABILITIES;
  return { ...defaults, ...overrides };
}
