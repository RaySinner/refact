import * as assert from "assert";
import * as fs from "fs";
import * as http from "http";
import * as net from "net";
import * as os from "os";
import * as path from "path";
import { disposeNow, engineBrowserUrl, engineUrl, isEngineRunning, killEngine, launchEngine } from "./engineProcess";

type FakeBehavior = {
    busy?: boolean;
    exitCode?: number;
    busyReport?: string;
};

function writeFakeEngine(dir: string, behavior: FakeBehavior): string {
    const script = [
        "const http = require('http');",
        `const behavior = ${JSON.stringify(behavior)};`,
        "const args = process.argv.slice(2);",
        "function argValue(name) { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; }",
        "const httpPort = Number(argValue('--http-port'));",
        "const pingMessage = argValue('--ping-message');",
        "if (behavior.busy) {",
        "    process.stdout.write((behavior.busyReport || JSON.stringify({workspace_busy:true,holder_pid:process.ppid,holder_http_port:0,holder_lsp_port:0})) + '\\n');",
        `    process.exit(${behavior.exitCode ?? 75});`,
        "}",
        "if (behavior.exitCode) { process.exit(behavior.exitCode); }",
        "const server = http.createServer((req, res) => {",
        "    if (req.url === '/v1/ping') {",
        "        res.end(pingMessage + '\\n');",
        "    } else if (req.url === '/v1/build_info') {",
        "        res.end(JSON.stringify({version: '8.7.9'}));",
        "    } else if (req.url === '/v1/graceful-shutdown') {",
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

function launchOptions(engineJsPath: string, dir: string) {
    return {
        binPath: process.execPath,
        root: dir,
        logDir: path.join(dir, "logs"),
        settings: { ast: true, astMaxFiles: 100, vecdb: false, vecdbMaxFiles: 0 },
        projectId: "test-project",
        argsPrefix: [engineJsPath],
        spawnTimeoutMs: 15000,
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

async function runBusyAdoptionTest(holderPort: number, dir: string) {
    const engine = writeFakeEngine(dir, {
        busy: true,
        busyReport: JSON.stringify({
            ["workspace_busy"]: true,
            ["holder_pid"]: process.pid,
            ["holder_http_port"]: holderPort,
            ["holder_lsp_port"]: 0,
        }),
    });
    const handle = await launchEngine(launchOptions(engine, dir));
    assert.strictEqual(handle.owned, false);
    assert.strictEqual(handle.httpPort, holderPort);
    assert.strictEqual(handle.lspPort, 0);
    assert.strictEqual(handle.pid, process.pid);
    await killEngine(handle);
}

async function runBusyAdoptionNullLspTest(holderPort: number, dir: string) {
    const engine = writeFakeEngine(dir, {
        busy: true,
        busyReport: JSON.stringify({
            ["workspace_busy"]: true,
            ["holder_pid"]: process.pid,
            ["holder_http_port"]: holderPort,
            ["holder_lsp_port"]: null,
        }),
    });
    const handle = await launchEngine(launchOptions(engine, dir));
    assert.strictEqual(handle.owned, false);
    assert.strictEqual(handle.lspPort, 0);
    await killEngine(handle);
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
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "engine-busy-"));
    const sockets = new Set<net.Socket>();
    const holder = http.createServer((req, res) => {
        if ((req.url || "").endsWith("/v1/build_info")) {
            res.end(JSON.stringify({ version: "8.7.9" }));
        } else {
            res.statusCode = 404;
            res.end("not found");
        }
    });
    holder.on("connection", socket => {
        sockets.add(socket);
        socket.once("close", () => sockets.delete(socket));
    });
    await new Promise<void>(resolve => holder.listen(0, "127.0.0.1", resolve));
    const holderPort = (holder.address() as net.AddressInfo).port;
    try {
        await runBusyAdoptionTest(holderPort, dir);
        await runBusyAdoptionNullLspTest(holderPort, dir);
        await runPortRetryTest(dir);
        await runMalformedBusyTest(dir);
    } finally {
        holder.closeAllConnections();
        await new Promise<void>(resolve => holder.close(() => resolve()));
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
