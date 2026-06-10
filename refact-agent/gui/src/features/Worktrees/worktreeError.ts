function textFromErrorData(data: unknown): string | null {
  if (typeof data === "string") return data;
  if (typeof data !== "object" || data === null) return null;
  if ("error" in data && typeof data.error === "string") return data.error;
  if ("detail" in data && typeof data.detail === "string") return data.detail;
  if ("message" in data && typeof data.message === "string") {
    return data.message;
  }
  return null;
}

function textFromEmbeddedJson(text: string): string | null {
  const start = text.indexOf("{");
  if (start === -1) return null;
  try {
    return textFromErrorData(JSON.parse(text.slice(start)) as unknown);
  } catch {
    return null;
  }
}

export function worktreeErrorText(error: unknown): string {
  if (typeof error === "object" && error !== null && "data" in error) {
    const dataText = textFromErrorData(error.data);
    if (dataText) return dataText;
  }
  if (error instanceof Error) {
    return textFromEmbeddedJson(error.message) ?? error.message;
  }
  if (typeof error === "object" && error !== null) {
    const directText = textFromErrorData(error);
    if (directText) return directText;
  }
  if (typeof error === "string") {
    return textFromEmbeddedJson(error) ?? error;
  }
  return String(error);
}
