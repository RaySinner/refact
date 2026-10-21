import * as assert from "assert";
import { spawn } from "child_process";
import * as fs from "fs";
import * as http from "http";
import * as net from "net";
import * as os from "os";
import * as path from "path";
import {
    defaultIsProcessRunning,
    disposeNow,
    engineBrowserUrl,
    engineUrl,
    isEngineRunning,
    killEngine,
    launchEngine,
    type EngineSpawnOptions,
} from "./engineProcess";

const PLUGIN_VERSION = "8.7.9";

type FakeBehavior = {
    busy?: boolean;
    exitCode?: number;
    busyReport?: string;
    answerPing?: boolean;
    graceful?: "ok" | "fail";
    version?: string;
    pidFile?: string;
    eventLog?: string;
};

function writeFakeEngine(dir: string, behavior: FakeBehavior): string {
    const script = [
        "const fs = require('fs');",
        "const http = require('http');",
        `const behavior = ${JSON.stringify(behavior)};`,
        "const args = process.argv.slice(2);",
        "function argValue(name) { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; }",
        "if (!args.includes('--no-mdns')) { console.error('missing --no-mdns'); process.exit(97); }",
        "const httpPort = Number(argValue('--http-port'));",
        "const pingMessage = argValue('--ping-message');",
        "function logEvent(text) { if (behavior.eventLog) { fs.appendFileSync(behavior.eventLog, text + '\\n'); } }",
        "if (behavior.pidFile) { fs.writeFileSync(behavior.pidFile, String(process.pid)); }",
        "if (behavior.eventLog) { process.on('exit', () => logEvent('exit')); }",
        "if (behavior.busy) {",
        "    process.stdout.write((behavior.busyReport || JSON.stringify({workspace_busy:true,holder_pid:process.ppid,holder_http_port:0,holder_lsp_port:0})) + '\\n');",
        `    process.exit(${behavior.exitCode ?? 75});`,
        "}",
        "if (behavior.exitCode) { process.exit(behavior.exitCode); }",
        "process.stdout.write('fake engine starting\\n');",
        "const server = http.createServer((req, res) => {",
        "    if (req.url === '/v1/ping') {",
        "        if (behavior.answerPing === false) { return; }",
        "        res.end(pingMessage + '\\n');",
        "    } else if (req.url === '/v1/build_info') {",
        "        res.end(JSON.stringify({version: behavior.version || '8.7.9'}));",
        "    } else if (req.url === '/v1/graceful-shutdown') {",
        "        if (behavior.graceful === 'fail') {",
        "            res.statusCode = 500;",
        "            res.end('cannot shutdown');",
        "            return;",
        "        }",
        "        logEvent('graceful');",
        "        res.end('ok');",
        "        process.exit(0);",
        "    } else {",
        "        res.statusCode = 404;",
        "        res.end('not found');",
        "    }",
        "});",
        "server.listen(httpPort, '127.0.0.1', () => {",
        "    process.stdout.write('listening\\n');",
        "});",
    ].join("\n");
    const filePath = path.join(dir, "fake-engine.js");
    fs.writeFileSync(filePath, script);
    return filePath;
}

function launchOptions(engineJsPath: string, dir: string, spawnTimeoutMs?: number): EngineSpawnOptions {
    return {
        binPath: process.execPath,
        root: dir,
        logDir: path.join(dir, "logs"),
        settings: { ast: true, astMaxFiles: 100, vecdb: false, vecdbMaxFiles: 0 },
        projectId: "test-project",
        pluginVersion: PLUGIN_VERSION,
        argsPrefix: [engineJsPath],
        spawnTimeoutMs: spawnTimeoutMs ?? 15000,
    };
}

function writeBusyEngine(dir: string, report: Record<string, unknown>): string {
    return writeFakeEngine(dir, { busy: true, busyReport: JSON.stringify(report) });
}

async function waitForFile(filePath: string, timeoutMs: number): Promise<number> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
        if (fs.existsSync(filePath)) {
            const raw = fs.readFileSync(filePath, "utf8").trim();
            if (raw.length > 0) {
                return Number.parseInt(raw, 10);
            }
        }
        await new Promise(resolve => setTimeout(resolve, 25));
    }
    throw new Error(`pid file ${filePath} was never written`);
}

async function waitForProcessExit(pid: number, timeoutMs: number): Promise<boolean> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
        if (!defaultIsProcessRunning(pid)) {
            return true;
        }
        await new Promise(resolve => setTimeout(resolve, 25));
    }
    return false;
}

function deadPid(): Promise<number> {
    const child = spawn(process.execPath, ["-e", "process.exit(0)"], { windowsHide: true });
    const pid = child.pid;
    return new Promise(resolve => {
        child.once("exit", () => resolve(pid ?? 0));
    });
}

async function unusedPort(): Promise<number> {
    const server = net.createServer();
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    const port = (server.address() as net.AddressInfo).port;
    await new Promise<void>(resolve => server.close(() => resolve()));
    return port;
}

async function startHolder(version: string | undefined): Promise<{ port: number; close: () => Promise<void> }> {
    const sockets = new Set<net.Socket>();
    const server = http.createServer((req, res) => {
        if ((req.url || "").endsWith("/v1/build_info")) {
            res.end(JSON.stringify(version === undefined ? {} : { version }));
        } else {
            res.statusCode = 404;
            res.end("not found");
        }
    });
    server.on("connection", socket => {
        sockets.add(socket);
        socket.once("close", () => sockets.delete(socket));
    });
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    const port = (server.address() as net.AddressInfo).port;
    return {
        port,
        close: () => new Promise<void>(resolve => {
            server.closeAllConnections();
            server.close(() => resolve());
        }),
    };
}

async function runLaunchReadyTest() {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "engine-ready-"));
    try {
        const engine = writeFakeEngine(dir, {});
        const handle = await launchEngine(launchOptions(engine, dir));
        assert.strictEqual(handle.owned, true);
        assert.notStrictEqual(handle.httpPort, handle.lspPort);
        assert.strictEqual(engineUrl(handle, "/v1/ping"), `http://127.0.0.1:${handle.httpPort}/v1/ping`);
        assert.strictEqual(engineBrowserUrl(handle), `http://127.0.0.1:${handle.httpPort}/`);
        assert.ok(isEngineRunning(handle));
        await killEngine(handle);
        assert.ok(!isEngineRunning(handle));
    } finally {
        fs.rmSync(dir, { recursive: true, force: true });
    }
}

async function runGracefulFailureKillsChildTest() {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "engine-graceful-fail-"));
    try {
        const pidFile = path.join(dir, "engine.pid");
        const engine = writeFakeEngine(dir, { graceful: "fail", pidFile });
        const handle = await launchEngine(launchOptions(engine, dir));
        assert.ok(isEngineRunning(handle));
        await killEngine(handle);
        const pid = await waitForFile(pidFile, 5000);
        assert.ok(await waitForProcessExit(pid, 5000), "engine that refuses graceful shutdown must be force killed");
        assert.ok(!isEngineRunning(handle));
    } finally {
        fs.rmSync(dir, { recursive: true, force: true });
    }
}

async function runKillOrderTest() {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "engine-kill-order-"));
    try {
        const eventLog = path.join(dir, "events.log");
        const engine = writeFakeEngine(dir, { eventLog });
        const handle = await launchEngine(launchOptions(engine, dir));
        await killEngine(handle);
        const events = fs.readFileSync(eventLog, "utf8").trim().split(/\r?\n/);
        assert.deepStrictEqual(events, ["graceful", "exit"], `graceful shutdown must run before any forced kill, got ${events.join(",")}`);
    } finally {
        fs.rmSync(dir, { recursive: true, force: true });
    }
}

async function runReadinessTimeoutLeavesNoChildTest() {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "engine-not-ready-"));
    try {
        const pidFile = path.join(dir, "engine.pid");
        const engine = writeFakeEngine(dir, { answerPing: false, pidFile });
        await assert.rejects(
            () => launchEngine(launchOptions(engine, dir, 800)),
            (error: Error) => {
                assert.match(error.message, /readiness timed out/);
                assert.match(error.message, /fake engine starting/);
                return true;
            },
        );
        const pid = await waitForFile(pidFile, 5000);
        assert.ok(await waitForProcessExit(pid, 5000), "a readiness timeout must not leak the spawned engine");
    } finally {
        fs.rmSync(dir, { recursive: true, force: true });
    }
}

async function runIncompatibleHolderTest(dir: string) {
    const holder = await startHolder("7.0.0");
    try {
        const engine = writeBusyEngine(dir, {
            ["workspace_busy"]: true,
            ["holder_pid"]: process.pid,
            ["holder_http_port"]: holder.port,
            ["holder_lsp_port"]: 65531,
        });
        await assert.rejects(
            () => launchEngine(launchOptions(engine, dir)),
            /incompatible engine version 7\.0\.0/,
        );
    } finally {
        await holder.close();
    }
}

async function runBusyAdoptionTest(dir: string) {
    const holder = await startHolder(PLUGIN_VERSION);
    try {
        const engine = writeBusyEngine(dir, {
            ["workspace_busy"]: true,
            ["holder_pid"]: process.pid,
            ["holder_http_port"]: holder.port,
            ["holder_lsp_port"]: 65532,
        });
        const handle = await launchEngine(launchOptions(engine, dir));
        assert.strictEqual(handle.owned, false);
        assert.strictEqual(handle.httpPort, holder.port);
        assert.strictEqual(handle.lspPort, 65532);
        assert.strictEqual(handle.pid, process.pid);
        await killEngine(handle);
    } finally {
        await holder.close();
    }
}

async function runHolderWithoutLspPortIsNotAdoptedTest(dir: string) {
    const holder = await startHolder(PLUGIN_VERSION);
    try {
        const engine = writeBusyEngine(dir, {
            ["workspace_busy"]: true,
            ["holder_pid"]: process.pid,
            ["holder_http_port"]: holder.port,
            ["holder_lsp_port"]: null,
        });
        await assert.rejects(
            () => launchEngine(launchOptions(engine, dir)),
            /holder LSP port is unknown/,
        );
    } finally {
        await holder.close();
    }
}

async function runHolderNotAnsweringRetriesTest(dir: string) {
    const engine = writeBusyEngine(dir, {
        ["workspace_busy"]: true,
        ["holder_pid"]: process.pid,
        ["holder_http_port"]: await unusedPort(),
        ["holder_lsp_port"]: 65533,
    });
    const startedAt = Date.now();
    await assert.rejects(
        () => launchEngine(launchOptions(engine, dir)),
        (error: Error) => {
            assert.match(error.message, /workspace stays busy, giving up after 3 attempts/);
            assert.doesNotMatch(error.message, /incompatible/);
            return true;
        },
    );
    assert.ok(Date.now() - startedAt >= 2500, "an unresponsive holder must be retried, not failed immediately");
}

async function runDyingHolderRetriesTest(dir: string) {
    const engine = writeBusyEngine(dir, {
        ["workspace_busy"]: true,
        ["holder_pid"]: await deadPid(),
        ["holder_http_port"]: 65534,
        ["holder_lsp_port"]: 65535,
    });
    await assert.rejects(
        () => launchEngine(launchOptions(engine, dir)),
        /workspace stays busy, giving up after 3 attempts/,
    );
}

async function runPortRetryTest(dir: string) {
    const engine = writeFakeEngine(dir, { exitCode: 48 });
    await assert.rejects(() => launchEngine(launchOptions(engine, dir)), /ports stayed busy/);
}

async function runMalformedBusyTest(dir: string) {
    const engine = writeFakeEngine(dir, { busy: true, busyReport: "{not-json" });
    await assert.rejects(() => launchEngine(launchOptions(engine, dir)), /failed|exited|busy|no output/);
}

async function runAll() {
    await runLaunchReadyTest();
    await runGracefulFailureKillsChildTest();
    await runKillOrderTest();
    await runReadinessTimeoutLeavesNoChildTest();
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "engine-busy-"));
    try {
        await runIncompatibleHolderTest(dir);
        await runBusyAdoptionTest(dir);
        await runHolderWithoutLspPortIsNotAdoptedTest(dir);
        await runHolderNotAnsweringRetriesTest(dir);
        await runDyingHolderRetriesTest(dir);
        await runPortRetryTest(dir);
        await runMalformedBusyTest(dir);
    } finally {
        try {
            fs.rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
        } catch {
        }
    }
    disposeNow({ httpPort: 0, lspPort: 0, owned: false });
}

runAll().then(() => {
    process.exit(0);
}).catch(error => {
    console.error(error);
    process.exit(1);
});
