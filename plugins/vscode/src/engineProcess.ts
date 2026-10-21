/* eslint-disable @typescript-eslint/naming-convention */
import { spawn, type ChildProcess } from "child_process";
import * as crypto from "crypto";
import * as fs from "fs";
import * as http from "http";
import * as net from "net";
import * as path from "path";
export function compareVersions(left: string | undefined, right: string | undefined): number {
    const leftParts = parseVersion(left);
    const rightParts = parseVersion(right);
    for (let i = 0; i < 3; i++) {
        const diff = leftParts.core[i] - rightParts.core[i];
        if (diff !== 0) {
            return diff > 0 ? 1 : -1;
        }
    }
    return comparePrerelease(leftParts.prerelease, rightParts.prerelease);
}

type ParsedVersion = {
    core: [number, number, number];
    prerelease?: string[];
};

function parseVersion(version: string | undefined): ParsedVersion {
    const trimmed = (version ?? "").trim();
    const withoutPrefix = trimmed.startsWith("v") ? trimmed.slice(1) : trimmed;
    const match = /(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?/.exec(withoutPrefix);
    if (!match) {
        return { core: [0, 0, 0] };
    }
    return {
        core: [toVersionNumber(match[1]), toVersionNumber(match[2]), toVersionNumber(match[3])],
        prerelease: parsePrerelease(match[4]),
    };
}

function toVersionNumber(part: string | undefined): number {
    return Number.parseInt(part ?? "0", 10) || 0;
}

function parsePrerelease(prerelease: string | undefined): string[] | undefined {
    const parts = prerelease?.split(".").filter(part => part.length > 0);
    return parts && parts.length > 0 ? parts : undefined;
}

function comparePrerelease(left: string[] | undefined, right: string[] | undefined): number {
    if (!left && !right) {
        return 0;
    }
    if (!left) {
        return 1;
    }
    if (!right) {
        return -1;
    }
    const length = Math.max(left.length, right.length);
    for (let i = 0; i < length; i++) {
        const leftPart = left[i];
        const rightPart = right[i];
        if (leftPart === undefined) {
            return -1;
        }
        if (rightPart === undefined) {
            return 1;
        }
        const diff = comparePrereleaseIdentifier(leftPart, rightPart);
        if (diff !== 0) {
            return diff;
        }
    }
    return 0;
}

function comparePrereleaseIdentifier(left: string, right: string): number {
    const leftNumeric = /^\d+$/.test(left);
    const rightNumeric = /^\d+$/.test(right);
    if (leftNumeric && rightNumeric) {
        const diff = Number.parseInt(left, 10) - Number.parseInt(right, 10);
        return diff === 0 ? 0 : diff > 0 ? 1 : -1;
    }
    if (leftNumeric) {
        return -1;
    }
    if (rightNumeric) {
        return 1;
    }
    return left === right ? 0 : left > right ? 1 : -1;
}

export function configuredBrowserHost(configuredHost?: string | null): string | undefined {
    const host = configuredHost?.trim();
    return host && host !== "0.0.0.0" ? host : undefined;
}

export function defaultIsProcessRunning(pid: number): boolean {
    if (!Number.isFinite(pid) || pid <= 0) {
        return false;
    }
    try {
        process.kill(pid, 0);
        return true;
    } catch (error) {
        const code = (error as NodeJS.ErrnoException).code;
        return code === "EPERM";
    }
}

export type PrimaryWorkspaceRootSelection = {
    primary?: string;
    ignored: string[];
    warning?: string;
};

export function selectPrimaryWorkspaceRoot(roots: string[]): PrimaryWorkspaceRootSelection {
    const primary = roots[0];
    const ignored = roots.slice(1);
    if (!primary || ignored.length === 0) {
        return { primary, ignored };
    }
    const suffix = ignored.length === 1 ? "folder" : "folders";
    return {
        primary,
        ignored,
        warning: `Refact serves only the primary VS Code workspace folder (${primary}); ignoring ${ignored.length} additional file workspace ${suffix}.`,
    };
}

export function missingBundledRefactError(assetPath: string): string {
    return `refact binary not found in ${assetPath} — reinstall the extension`;
}

function bundledRefactName(): string {
    return process.platform === "win32" ? "refact.exe" : "refact";
}

export function resolveBundledRefactPath(assetPath: string): string {
    return path.join(assetPath, bundledRefactName());
}

export function ensureBundledRefactPath(assetPath: string): string {
    const binPath = resolveBundledRefactPath(assetPath);
    if (!fs.existsSync(binPath)) {
        throw new Error(missingBundledRefactError(assetPath));
    }
    return binPath;
}

type Sha256CacheEntry = {
    size: number;
    mtimeMs: number;
    sha256: string;
};

const sha256Cache = new Map<string, Sha256CacheEntry>();

export async function sha256OfFile(filePath: string): Promise<string | undefined> {
    const absolutePath = path.resolve(filePath);
    try {
        const stat = await fs.promises.stat(absolutePath);
        if (!stat.isFile()) {
            return undefined;
        }
        const cached = sha256Cache.get(absolutePath);
        if (cached && cached.size === stat.size && cached.mtimeMs === stat.mtimeMs) {
            return cached.sha256;
        }
        const sha256 = await sha256File(absolutePath);
        sha256Cache.set(absolutePath, { size: stat.size, mtimeMs: stat.mtimeMs, sha256 });
        return sha256;
    } catch {
        return undefined;
    }
}

function sha256File(filePath: string): Promise<string> {
    return new Promise((resolve, reject) => {
        const hash = crypto.createHash("sha256");
        const stream = fs.createReadStream(filePath);
        stream.on("data", chunk => hash.update(chunk));
        stream.once("error", reject);
        stream.once("end", () => resolve(hash.digest("hex")));
    });
}

export type EngineHandle = { httpPort: number; lspPort: number; owned: boolean; pid?: number };
export type EngineSpawnOptions = {
    binPath: string;
    root: string;
    logDir: string;
    settings: { ast: boolean; astMaxFiles: number; vecdb: boolean; vecdbMaxFiles: number };
    projectId: string;
    pluginVersion: string;
    spawnTimeoutMs?: number;
    debug?: { httpPort: number; lspPort: number };
    argsPrefix?: string[];
};
const ENGINE_READY_TIMEOUT_MS = 120000;
const ENGINE_POLL_MS = 250;
const ENGINE_PORT_BUSY_ATTEMPTS = 3;
const ENGINE_BUSY_RETRY_DELAYS_MS = [1000, 2000];
const ENGINE_GRACEFUL_TIMEOUT_MS = 5000;
const ENGINE_EXIT_WAIT_MS = 10000;
const ENGINE_KILL_WAIT_MS = 5000;
const ENGINE_TAIL_BYTES = 12000;
const ENGINE_BUSY_EXIT_CODE = 75;

const portBusyExitCodes = new Set([48, 98, 10048]);
const engineProcesses = new Map<string, ChildProcess>();

export async function launchEngine(options: EngineSpawnOptions): Promise<EngineHandle> {
    if (options.debug) {
        return { httpPort: options.debug.httpPort, lspPort: options.debug.lspPort, owned: false };
    }
    const deadline = Date.now() + (options.spawnTimeoutMs ?? ENGINE_READY_TIMEOUT_MS);
    let portBusyAttempts = 0;
    let busyRetries = 0;
    let lastError = "engine did not become ready";
    while (Date.now() < deadline) {
        const ports = await allocatePortPair();
        const attempt = await spawnAndWaitReady(options, ports.httpPort, ports.lspPort, deadline);
        if (attempt.outcome === "ready") {
            return attempt.handle as EngineHandle;
        }
        lastError = attempt.error as string;
        if (attempt.busyReport) {
            const resolution = await resolveBusy(options, attempt.busyReport);
            if (resolution.outcome === "adopted") {
                return resolution.handle;
            }
            if (resolution.outcome === "error") {
                throw new Error(resolution.message);
            }
            if (busyRetries >= ENGINE_BUSY_RETRY_DELAYS_MS.length) {
                throw new Error(`workspace stays busy, giving up after ${busyRetries + 1} attempts: ${lastError}`);
            }
            await delay(ENGINE_BUSY_RETRY_DELAYS_MS[busyRetries]);
            busyRetries++;
            continue;
        }
        if (attempt.portBusy) {
            portBusyAttempts++;
            if (portBusyAttempts >= ENGINE_PORT_BUSY_ATTEMPTS) {
                throw new Error(`engine ports stayed busy after ${portBusyAttempts} attempts: ${lastError}`);
            }
            continue;
        }
        throw new Error(lastError);
    }
    throw new Error(lastError);
}

export async function killEngine(handle: EngineHandle): Promise<void> {
    if (!handle.owned) {
        return;
    }
    const key = engineKey(handle);
    const child = engineProcesses.get(key);
    engineProcesses.delete(key);
    if (child && (child.exitCode !== null || child.signalCode !== null)) {
        return;
    }
    const pid = handle.pid ?? child?.pid;
    try {
        await engineRequest(handle.httpPort, "/v1/graceful-shutdown", "POST", ENGINE_GRACEFUL_TIMEOUT_MS);
    } catch {
        // The engine did not answer: that is exactly when the forced kill is needed.
    }
    if (child) {
        if (await waitForExit(child, ENGINE_EXIT_WAIT_MS)) {
            return;
        }
        child.kill();
        if (await waitForExit(child, ENGINE_KILL_WAIT_MS)) {
            return;
        }
    }
    if (pid && process.platform === "win32") {
        await windowsTaskKill(pid);
        if (child && (await waitForExit(child, ENGINE_KILL_WAIT_MS))) {
            return;
        }
    }
    console.log(`Refact engine (pid ${pid ?? "unknown"}) survived the kill ladder`);
}

export function disposeNow(handle: EngineHandle): void {
    if (!handle.owned) {
        return;
    }
    const key = engineKey(handle);
    const child = engineProcesses.get(key);
    engineProcesses.delete(key);
    if (child && child.exitCode === null && child.signalCode === null) {
        child.kill();
    }
}

export function engineUrl(handle: { httpPort: number }, addthis: string): string {
    const suffix = addthis.startsWith("/") ? addthis : `/${addthis}`;
    return `http://127.0.0.1:${handle.httpPort}${suffix}`;
}

export function engineBrowserUrl(handle: { httpPort: number }, configuredHost?: string): string {
    const host = configuredBrowserHost(configuredHost) ?? "127.0.0.1";
    return `http://${host}:${handle.httpPort}/`;
}

export function isEngineRunning(handle: EngineHandle): boolean {
    if (!handle.owned || !handle.pid) {
        return false;
    }
    return defaultIsProcessRunning(handle.pid);
}

async function allocatePortPair(): Promise<{ httpPort: number; lspPort: number }> {
    for (let i = 0; i < 16; i++) {
        const httpPort = await allocatePort();
        const lspPort = await allocatePort();
        if (httpPort !== lspPort) {
            return { httpPort, lspPort };
        }
    }
    throw new Error("failed to allocate distinct engine ports");
}

function allocatePort(): Promise<number> {
    return new Promise((resolve, reject) => {
        const server = net.createServer();
        server.unref();
        server.on("error", reject);
        server.listen(0, "127.0.0.1", () => {
            const address = server.address();
            const port = address && typeof address === "object" ? address.port : 0;
            server.close(() => port > 0 ? resolve(port) : reject(new Error("failed to allocate port")));
        });
    });
}

type SpawnAttempt = {
    outcome: "ready" | "failed";
    handle?: EngineHandle;
    error?: string;
    busyReport?: BusyReport;
    portBusy?: boolean;
};

function engineArgs(options: EngineSpawnOptions, httpPort: number, lspPort: number, nonce: string): string[] {
    const args = [
        ...(options.argsPrefix ?? []),
        "worker",
        "--workspace-folder", options.root,
        "--http-port", String(httpPort),
        "--http-host", "127.0.0.1",
        "--lsp-port", String(lspPort),
        "--ping-message", nonce,
        "--project-id", options.projectId,
        "--logs-to-file", path.join(options.logDir, "engine.log"),
        "--watch-parent-stdin",
    ];
    if (options.settings.ast) {
        args.push("--ast", "--ast-max-files", String(options.settings.astMaxFiles));
    }
    if (options.settings.vecdb) {
        args.push("--vecdb", "--vecdb-max-files", String(options.settings.vecdbMaxFiles));
    }
    return args;
}

function spawnAndWaitReady(
    options: EngineSpawnOptions,
    httpPort: number,
    lspPort: number,
    deadline: number,
): Promise<SpawnAttempt> {
    const nonce = crypto.randomUUID();
    return new Promise(resolve => {
        let settled = false;
        let stdoutTail = "";
        let stderrTail = "";
        let pollTimer: NodeJS.Timeout | undefined;
        let child: ChildProcess | undefined;
        const finish = (attempt: SpawnAttempt) => {
            if (settled) {
                return;
            }
            settled = true;
            if (pollTimer) {
                clearInterval(pollTimer);
            }
            if (attempt.outcome === "ready" || !child) {
                resolve(attempt);
                return;
            }
            void discardChild(child).then(() => resolve(attempt));
        };
        let spawned: ChildProcess;
        try {
            fs.mkdirSync(options.logDir, { recursive: true });
            spawned = spawn(options.binPath, engineArgs(options, httpPort, lspPort, nonce), {
                cwd: options.root,
                detached: false,
                windowsHide: true,
                stdio: ["pipe", "pipe", "pipe"],
            });
        } catch (error) {
            resolve({ outcome: "failed", error: errorMessage(error) });
            return;
        }
        child = spawned;
        child.stdout?.on("data", (chunk: Buffer) => {
            stdoutTail = appendTail(stdoutTail, chunk);
        });
        child.stderr?.on("data", (chunk: Buffer) => {
            stderrTail = appendTail(stderrTail, chunk);
        });
        child.once("error", (error: Error) => {
            finish({ outcome: "failed", error: errorMessage(error) });
        });
        child.once("exit", (code: number | null) => {
            const tail = tailMessage(stdoutTail, stderrTail);
            if (code !== null && code === ENGINE_BUSY_EXIT_CODE) {
                const busy = extractBusyReport(stdoutTail);
                if (busy) {
                    finish({ outcome: "failed", busyReport: busy, error: tail });
                    return;
                }
            }
            if (code !== null && portBusyExitCodes.has(code)) {
                finish({ outcome: "failed", portBusy: true, error: tail });
                return;
            }
            finish({ outcome: "failed", error: `engine exited before ready (code ${code}): ${tail}` });
        });
        pollTimer = setInterval(() => {
            if (Date.now() >= deadline) {
                finish({ outcome: "failed", error: `engine readiness timed out: ${tailMessage(stdoutTail, stderrTail)}` });
                return;
            }
            engineRequest(httpPort, "/v1/ping", "GET", ENGINE_POLL_MS)
                .then(body => {
                    if (body.trim() === nonce) {
                        engineProcesses.set(engineKey({ httpPort, lspPort, owned: true }), spawned);
                        finish({ outcome: "ready", handle: { httpPort, lspPort, owned: true, pid: spawned.pid ?? undefined } });
                    }
                })
                .catch(() => undefined);
        }, ENGINE_POLL_MS);
    });
}

async function discardChild(child: ChildProcess): Promise<void> {
    if (!child.pid || child.exitCode !== null || child.signalCode !== null) {
        return;
    }
    child.kill();
    if (await waitForExit(child, ENGINE_KILL_WAIT_MS)) {
        return;
    }
    if (child.pid && process.platform === "win32") {
        await windowsTaskKill(child.pid);
        await waitForExit(child, ENGINE_KILL_WAIT_MS);
    }
}

type BusyReport = {
    holder_pid?: number | null;
    holder_http_port?: number | null;
    holder_lsp_port?: number | null;
};

type BusyResolution =
    | { outcome: "adopted"; handle: EngineHandle }
    | { outcome: "retry" }
    | { outcome: "error"; message: string };

async function resolveBusy(options: EngineSpawnOptions, report: BusyReport): Promise<BusyResolution> {
    const pid = normalizeId(report.holder_pid);
    const httpPort = normalizeId(report.holder_http_port);
    const lspPort = normalizeId(report.holder_lsp_port);
    if (!pid || !defaultIsProcessRunning(pid)) {
        return { outcome: "retry" };
    }
    if (!httpPort) {
        return { outcome: "error", message: busyHolderError(pid, undefined, lspPort, "http port unknown") };
    }
    const version = await holderVersion(httpPort);
    if (!version) {
        return { outcome: "retry" };
    }
    if (compareVersions(version, options.pluginVersion) < 0) {
        return { outcome: "error", message: busyHolderError(pid, httpPort, lspPort, `incompatible engine version ${version}, this extension needs ${options.pluginVersion} or newer`) };
    }
    if (!lspPort) {
        return { outcome: "error", message: busyHolderError(pid, httpPort, undefined, "holder LSP port is unknown; restart the window that holds this workspace") };
    }
    return { outcome: "adopted", handle: { httpPort, lspPort, owned: false, pid } };
}

function busyHolderError(pid: number, httpPort: number | undefined, lspPort: number | undefined, reason: string): string {
    const ports = `http port ${httpPort ?? "unknown"}, lsp port ${lspPort ?? "unknown"}`;
    return `workspace is held by another engine (pid ${pid}, ${ports}): ${reason}`;
}

async function holderVersion(httpPort: number): Promise<string | undefined> {
    try {
        const info = await engineRequest(httpPort, "/v1/build_info", "GET", 2000);
        const version = (JSON.parse(info) as { version?: unknown }).version;
        return typeof version === "string" && version.trim().length > 0 ? version.trim() : undefined;
    } catch {
        return undefined;
    }
}

function normalizeId(value: number | null | undefined): number | undefined {
    if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
        return undefined;
    }
    return Math.trunc(value);
}

function extractBusyReport(stdout: string): BusyReport | undefined {
    const lines = stdout.split(/\r?\n/).filter(line => line.includes("workspace_busy"));
    for (const line of lines.reverse()) {
        try {
            const parsed = JSON.parse(line.trim()) as BusyReport & { workspace_busy?: unknown };
            if (parsed && typeof parsed === "object" && parsed.workspace_busy === true) {
                return parsed;
            }
        } catch {
            continue;
        }
    }
    return undefined;
}

function appendTail(current: string, chunk: Buffer | string): string {
    const next = current + chunk.toString();
    return next.length > ENGINE_TAIL_BYTES ? next.slice(-ENGINE_TAIL_BYTES) : next;
}

function tailMessage(stdout: string, stderr: string): string {
    const combined = [stdout.trim(), stderr.trim()].filter(part => part.length > 0).join("\n");
    return combined || "no output";
}

function engineRequest(port: number, pathname: string, method: string, timeoutMs: number): Promise<string> {
    return new Promise((resolve, reject) => {
        if (!port) {
            reject(new Error("engine port is not available"));
            return;
        }
        const request = http.request(
            {
                host: "127.0.0.1",
                port,
                path: pathname,
                method,
                timeout: timeoutMs,
            },
            response => {
                let body = "";
                response.setEncoding("utf8");
                response.on("data", chunk => {
                    body += chunk;
                });
                response.on("end", () => {
                    if (response.statusCode && response.statusCode >= 200 && response.statusCode < 300) {
                        resolve(body);
                    } else {
                        reject(new Error(`engine request failed ${response.statusCode} ${pathname}: ${body}`));
                    }
                });
            },
        );
        request.on("timeout", () => {
            request.destroy();
            reject(new Error(`engine request timed out ${pathname}`));
        });
        request.on("error", reject);
        request.end();
    });
}

function waitForExit(child: ChildProcess, timeoutMs: number): Promise<boolean> {
    if (child.exitCode !== null || child.signalCode !== null) {
        return Promise.resolve(true);
    }
    return new Promise(resolve => {
        const timer = setTimeout(() => {
            cleanup();
            resolve(false);
        }, timeoutMs);
        const onExit = () => {
            cleanup();
            resolve(true);
        };
        const cleanup = () => {
            clearTimeout(timer);
            child.removeListener("exit", onExit);
        };
        child.once("exit", onExit);
    });
}

function windowsTaskKill(pid: number): Promise<void> {
    return new Promise(resolve => {
        const child = spawn("taskkill", ["/T", "/F", "/PID", String(pid)], { windowsHide: true });
        child.once("error", () => resolve());
        child.once("exit", () => resolve());
    });
}

function engineKey(handle: EngineHandle): string {
    return `${handle.httpPort}:${handle.lspPort}`;
}

function delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
}

function errorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
}
