/* eslint-disable @typescript-eslint/naming-convention */
import * as vscode from 'vscode';
import * as fetchH2 from 'fetch-h2';
import * as fetchAPI from "./fetchAPI";
import * as lspClient from 'vscode-languageclient/node';
import * as net from 'net';
import * as path from 'path';
import { register_commands } from './rconsoleCommands';
import { QuickActionProvider } from './quickProvider';
import * as os from 'os';
import * as refactBinary from './refactBinaryResolver';
import * as engineProcess from './engineProcess';
import { registerSharedRefactPath } from './refactPathRegistration';
import { backendReadyForStatus, type RefactBackendConnectionStatus } from './backendStatus';
import {
    lspClientReadyTimeoutMs,
    lspClientStopTimeoutMs,
    lspSocketCloseAction,
    lspSocketConnectTimeoutMs,
    attachStateForEngineLaunch,
    browserUrlForBackendStatus,
    shouldRunLifecycleGeneration,
} from './launchRustLifecycle';
import { WorkerHealthProbe } from './launchRustHealth';

const DEBUG_HTTP_PORT = 8001;
const DEBUG_LSP_PORT = 8002;

type ProjectProxyRequestInit = Omit<Partial<fetchH2.RequestInit>, "headers"> & {
    headers?: Record<string, string>;
};

export class RustBinaryBlob {
    public asset_path: string;
    public binary_cache_path: string;
    public port: number = 0;
    public lsp_port: number = 0;
    public owned_engine: engineProcess.EngineHandle | undefined = undefined;
    public lsp_disposable: vscode.Disposable | undefined = undefined;
    public lsp_client: lspClient.LanguageClient | undefined = undefined;
    public lsp_socket: net.Socket | undefined = undefined;
    public lsp_client_options: lspClient.LanguageClientOptions;
    private lifecycleQueue: Promise<void> = Promise.resolve();
    private lifecycleGeneration: number = 0;
    private reconnectGeneration: number | undefined = undefined;
    private reconnectTimer: NodeJS.Timeout | undefined = undefined;
    private reconnectAttempts: number = 0;
    private attachState: RefactBackendConnectionStatus = "connecting";
    private workerHealthProbe: WorkerHealthProbe;
    private warned_engine_fallbacks: Set<string> = new Set();

    constructor(asset_path: string, binary_cache_path?: string) {
        this.asset_path = asset_path;
        this.binary_cache_path = binary_cache_path ?? path.join(asset_path, "refact-bin");
        this.workerHealthProbe = new WorkerHealthProbe({
            probe: () => this.probe_attached_worker(),
            recover: () => this.recover_unhealthy_worker(),
        });
        this.lsp_client_options = {
            documentSelector: [{ scheme: 'file', language: '*' }],
            diagnosticCollectionName: 'RUST LSP',
            progressOnInitialization: true,
            traceOutputChannel: vscode.window.createOutputChannel('RUST LSP'),
            revealOutputChannelOn: lspClient.RevealOutputChannelOn.Error,
        };
    }

    public x_debug(): number {
        let xdebug = vscode.workspace.getConfiguration().get("refactai.xDebug");
        if (xdebug === undefined || xdebug === null || xdebug === 0 || xdebug === "0" || xdebug === false || xdebug === "false") {
            return 0;
        }
        return 1;
    }

    public get_port(): number {
        if (this.x_debug()) {
            return DEBUG_HTTP_PORT;
        }
        return this.port;
    }

    public rust_url(): string {
        if (this.x_debug()) {
            return `http2://127.0.0.1:${DEBUG_HTTP_PORT}/`;
        }
        if (!this.port) {
            return "";
        }
        return `http://127.0.0.1:${this.port}/`;
    }

    public backend_status(): RefactBackendConnectionStatus {
        return this.attachState;
    }

    public backend_ready(): boolean {
        return backendReadyForStatus(this.attachState);
    }

    public set_backend_status_for_test(status: RefactBackendConnectionStatus) {
        this.set_attach_state(status);
    }

    private set_attach_state(status: RefactBackendConnectionStatus) {
        if (this.attachState === status) {
            return;
        }
        this.attachState = status;
        global.side_panel?.handleSettingsChange();
        global.open_chat_tabs?.forEach(tab => tab.handleSettingsChange());
        global.status_bar?.choose_color();
    }

    private is_current_generation(generation: number): boolean {
        return shouldRunLifecycleGeneration(generation, this.lifecycleGeneration);
    }

    private dispose_lsp_resources(
        socket: net.Socket,
        client?: lspClient.LanguageClient,
        disposable?: vscode.Disposable,
    ): void {
        const currentSocket = this.lsp_socket === socket;
        if (disposable) {
            disposable.dispose();
            if (this.lsp_disposable === disposable) {
                this.lsp_disposable = undefined;
            }
        } else if (currentSocket && this.lsp_disposable) {
            this.lsp_disposable.dispose();
            this.lsp_disposable = undefined;
        }
        if (client) {
            if (this.lsp_client === client) {
                this.lsp_client = undefined;
            }
        } else if (currentSocket) {
            this.lsp_client = undefined;
        }
        if (currentSocket) {
            this.lsp_socket = undefined;
        }
        if (!socket.destroyed) {
            socket.destroy();
        }
    }

    private mark_lsp_disconnected(): void {
        this.workerHealthProbe.stop();
        global.have_caps = false;
        this.set_attach_state("connecting");
    }

    private mark_lsp_ready(): void {
        this.set_attach_state("ready");
        this.reconnectAttempts = 0;
        if (!this.x_debug()) {
            this.workerHealthProbe.start();
        }
    }

    private mark_worker_unhealthy(): void {
        this.lsp_dispose();
        global.have_caps = false;
        this.set_attach_state("failed");
    }

    private async probe_attached_worker(): Promise<boolean> {
        if (this.x_debug() || this.attachState !== "ready" || !this.port) {
            return true;
        }
        try {
            const resp = await this.fetch_project_proxy_once("/v1/build_info", {
                method: "GET",
                redirect: "follow",
                cache: "no-cache",
                referrer: "no-referrer",
            }, { timeout: 5000 });
            if (resp.status !== 200) {
                console.log(["worker health http status", resp.status]);
                return false;
            }
            return true;
        } catch (error) {
            console.log(["worker health error", error]);
            return false;
        }
    }

    private recover_unhealthy_worker(): void {
        if (this.x_debug() || !this.is_current_generation(this.lifecycleGeneration)) {
            return;
        }
        console.log("RUST worker health failed repeatedly; scheduling engine reconnect");
        this.mark_worker_unhealthy();
        this.reconnectGeneration = this.lifecycleGeneration;
        this.schedule_lsp_reconnect(this.lifecycleGeneration);
    }

    public browser_url(): string {
        const configuredHost = vscode.workspace.getConfiguration().get<string>("refactai.browserHost")?.trim();
        return browserUrlForBackendStatus({
            status: this.attachState,
            debug: Boolean(this.x_debug()),
            debugHttpPort: DEBUG_HTTP_PORT,
            port: this.port,
            configuredHost,
        });
    }

    public attemping_to_reach(): string {
        if (this.x_debug()) {
            return `debug rust binary on ports ${DEBUG_HTTP_PORT} and ${DEBUG_LSP_PORT}`;
        }
        if (this.port) {
            return `Refact engine on port ${this.port}`;
        }
        return `Refact engine`;
    }

    public async settings_changed() {
        return this.enqueueLifecycle(async (generation) => this.settings_changed_serialized(generation));
    }

    private enqueueLifecycle(operation: (generation: number) => Promise<void>): Promise<void> {
        const generation = ++this.lifecycleGeneration;
        const run = this.lifecycleQueue.catch(() => undefined).then(() => {
            if (!this.is_current_generation(generation)) {
                return undefined;
            }
            return operation(generation);
        });
        this.lifecycleQueue = run.catch(() => undefined);
        return run;
    }

    private async settings_changed_serialized(generation: number) {
        this.workerHealthProbe.stop();
        this.set_attach_state("connecting");
        global.status_bar?.set_socket_error(false, "");
        try {
            if (this.x_debug()) {
                await this.attach_debug_serialized(generation);
            } else {
                await this.attach_engine_serialized(generation);
            }
        } catch (error) {
            if (!this.is_current_generation(generation)) {
                return;
            }
            console.log(["Refact attach failed", error]);
            global.have_caps = false;
            await this.reset_engine_state();
            this.set_attach_state("failed");
            global.status_bar.set_socket_error(true, error instanceof Error ? error.message : String(error));
        } finally {
            if (this.is_current_generation(generation)) {
                global.side_panel?.handleSettingsChange();
            }
        }
    }

    public async launch() {
        return this.enqueueLifecycle(async (generation) => this.launch_serialized(generation));
    }

    private async launch_serialized(generation: number) {
        await this.settings_changed_serialized(generation);
    }

    private async attach_debug_serialized(generation: number) {
        this.clearReconnectTimer();
        await this.stop_lsp(generation);
        if (!this.is_current_generation(generation)) {
            return;
        }
        await this.reset_engine_state();
        this.port = DEBUG_HTTP_PORT;
        this.lsp_port = DEBUG_LSP_PORT;
        console.log(`RUST debug is set, don't start the rust binary. Will attempt HTTP port ${DEBUG_HTTP_PORT}, LSP port ${DEBUG_LSP_PORT}`);
        console.log("Also, will try to read caps. If that fails, things like lists of available models will be empty.");
        await this.start_lsp_socket(generation);
    }

    private async attach_engine_serialized(generation: number) {
        this.clearReconnectTimer();
        await this.stop_lsp(generation);
        if (!this.is_current_generation(generation)) {
            return;
        }

        const roots = this.workspace_roots();
        const rootSelection = engineProcess.selectPrimaryWorkspaceRoot(roots);
        if (!rootSelection.primary) {
            await this.reset_engine_state();
            throw new Error("Open a workspace folder before starting Refact.");
        }
        this.warn_about_ignored_roots(rootSelection);

        const pluginVersion = this.plugin_version();
        const configuredBinary = vscode.workspace.getConfiguration().get<string>("refactai.binaryPath")?.trim();
        const resolved = await refactBinary.resolveRefactBinaryDetailed({
            explicitPath: configuredBinary,
            bundledDir: this.asset_path,
            minVersion: pluginVersion,
            pinnedVersion: pluginVersion,
            cacheDir: this.binary_cache_path,
            onDownloadStart: () => {
                if (this.is_current_generation(generation)) {
                    this.set_attach_state("installing");
                }
            },
            onFallbackVersion: (pinned, chosen) => this.warn_engine_version_fallback(pinned, chosen),
        });
        if (!this.is_current_generation(generation)) {
            return;
        }
        this.maybe_register_shared_refact_path(resolved.binPath);
        this.set_attach_state(attachStateForEngineLaunch());

        // Kill prior engine before launching new one
        await this.reset_engine_state();

        const logDir = path.join(global.global_context?.globalStorageUri?.fsPath ?? this.binary_cache_path, "logs");
        const settings = this.engine_settings();
        const root = rootSelection.primary;
        const projectId = path.basename(root);

        const handle = await engineProcess.launchEngine({
            binPath: resolved.binPath,
            root,
            logDir,
            settings,
            projectId,
            pluginVersion,
        });

        if (!this.is_current_generation(generation)) {
            await engineProcess.killEngine(handle);
            return;
        }

        this.port = handle.httpPort;
        this.lsp_port = handle.lspPort;
        this.owned_engine = handle;
        await this.start_lsp_socket(generation);
    }

    public async stop_lsp(generation: number = this.lifecycleGeneration) {
        if (this.is_current_generation(generation)) {
            this.workerHealthProbe.stop();
            this.reconnectGeneration = undefined;
            this.clearReconnectTimer();
        }
        const my_lsp_client_copy = this.lsp_client;
        if (my_lsp_client_copy) {
            console.log("RUST STOP");
            const ts = Date.now();
            try {
                await Promise.race([
                    my_lsp_client_copy.stop(),
                    new Promise<void>(resolve => setTimeout(resolve, lspClientStopTimeoutMs)),
                ]);
                console.log(`RUST /STOP completed in ${Date.now() - ts}ms`);
            } catch (e) {
                console.log(`RUST STOP ERROR e=${e}`);
            } finally {
                console.log("RUST STOP FINALLY");
            }
        }
        if (this.is_current_generation(generation)) {
            this.lsp_dispose();
        }
    }

    public lsp_dispose() {
        this.workerHealthProbe.stop();
        if (this.lsp_disposable) {
            this.lsp_disposable.dispose();
            this.lsp_disposable = undefined;
        }
        const socket = this.lsp_socket;
        this.lsp_client = undefined;
        this.lsp_socket = undefined;
        if (socket && !socket.destroyed) {
            socket.destroy();
        }
    }

    public async terminate() {
        this.workerHealthProbe.dispose();
        this.lifecycleGeneration++;
        this.reconnectGeneration = undefined;
        this.clearReconnectTimer();
        await this.stop_lsp(this.lifecycleGeneration);
        await this.reset_engine_state();
        await fetchH2.disconnectAll();
        global.have_caps = false;
        global.status_bar.choose_color();
        this.set_attach_state("connecting");
    }

    public async fetch_project_proxy(
        addthis: string,
        init: ProjectProxyRequestInit,
        fetchInit?: Partial<fetchH2.FetchInit>,
    ): Promise<fetchH2.Response> {
        return this.fetch_project_proxy_once(addthis, init, fetchInit);
    }

    public project_proxy_url(addthis: string): string {
        return this.engine_url(addthis);
    }

    public engine_url(addthis: string): string {
        let url = this.rust_url();
        if (!url) {
            return "";
        }
        while (url.endsWith("/")) {
            url = url.slice(0, -1);
        }
        const suffix = addthis.startsWith("/") ? addthis : `/${addthis}`;
        return url + suffix;
    }

    private async fetch_project_proxy_once(
        addthis: string,
        init: ProjectProxyRequestInit,
        fetchInit?: Partial<fetchH2.FetchInit>,
    ): Promise<fetchH2.Response> {
        const url = this.engine_url(addthis);
        if (!url) {
            return Promise.reject("No rust binary working");
        }
        const request = new fetchH2.Request(url, {
            ...init,
            headers: init.headers,
        });
        return fetchH2.fetch(request, fetchInit);
    }

    private warn_about_ignored_roots(selection: { primary?: string; ignored: string[]; warning?: string }): void {
        if (!selection.warning) {
            return;
        }
        console.log(selection.warning);
        void vscode.window.showWarningMessage(selection.warning);
    }

    private async reset_engine_state(): Promise<void> {
        const handle = this.owned_engine;
        this.owned_engine = undefined;
        this.lsp_port = 0;
        this.port = 0;
        if (handle) {
            await engineProcess.killEngine(handle);
        }
    }

    public async read_caps() {
        try {
            const resp = await this.fetch_project_proxy("/v1/caps", {
                method: "GET",
                redirect: "follow",
                cache: "no-cache",
                referrer: "no-referrer"
            }, { timeout: 5000 });
            if (resp.status !== 200) {
                console.log(["read_caps http status", resp.status]);
                return Promise.reject("read_caps bad status");
            }
            let json = await resp.json();
            console.log(["successful read_caps", json]);
            global.chat_models = Object.keys(json["chat_models"]);
            global.chat_default_model = json["chat_default_model"] || "";
            global.have_caps = true;
            global.status_bar.set_socket_error(false, "");
        } catch (e) {
            global.chat_models = [];
            global.have_caps = false;
            console.log(["read_caps:", e]);
        }
        global.status_bar.choose_color();
        fetchAPI.maybe_show_rag_status();
        let current_editor = vscode.window.activeTextEditor;
        if (current_editor) {
            fetchAPI.lsp_set_active_document(current_editor);
        }

        const promptCustomization = await fetchAPI.get_prompt_customization().catch(error => {
            console.log(["get_prompt_customization", error]);
            return undefined;
        });
        if (promptCustomization && promptCustomization.toolbox_commands) {
            await QuickActionProvider.updateActions(promptCustomization.toolbox_commands as Record<string, ToolboxCommand>);
        }
    }

    public async ping() {
        try {
            let resp = await this.fetch_project_proxy("/v1/ping", {
                method: "GET",
                redirect: "follow",
                cache: "no-cache",
                referrer: "no-referrer",
            }, { timeout: 5000 });
            if (resp.status !== 200) {
                console.log(["ping http status", resp.status]);
                return false;
            }
            return true;
        } catch (e) {
            console.log(["ping error:", e]);
        }
        return false;
    }

    private async wait_lsp_client_ready(client: lspClient.LanguageClient): Promise<void> {
        let readyTimer: NodeJS.Timeout | undefined;
        try {
            await Promise.race([
                client.onReady(),
                new Promise<void>((_resolve, reject) => {
                    readyTimer = setTimeout(() => {
                        reject(new Error(`RUST LSP client onReady timed out after ${lspClientReadyTimeoutMs}ms`));
                    }, lspClientReadyTimeoutMs);
                }),
            ]);
        } finally {
            if (readyTimer) {
                clearTimeout(readyTimer);
            }
        }
    }

    public async start_lsp_socket(generation: number = this.lifecycleGeneration) {
        if (!this.is_current_generation(generation)) {
            return;
        }
        const lspPort = this.x_debug() ? DEBUG_LSP_PORT : this.lsp_port;
        if (!lspPort) {
            throw new Error("Refact LSP port is not available.");
        }
        console.log(`RUST start_lsp_socket ${lspPort}`);
        this.reconnectGeneration = this.x_debug() ? undefined : generation;

        await new Promise<void>((resolve, reject) => {
            let settled = false;
            let connectTimer: NodeJS.Timeout | undefined;
            const finish = (error?: Error) => {
                if (settled) {
                    return;
                }
                settled = true;
                if (connectTimer) {
                    clearTimeout(connectTimer);
                    connectTimer = undefined;
                }
                if (error) {
                    reject(error);
                    return;
                }
                resolve();
            };
            const fail_or_reconnect = (socket: net.Socket, error: Error) => {
                this.dispose_lsp_resources(socket);
                if (!this.is_current_generation(generation)) {
                    finish();
                    return;
                }
                this.mark_lsp_disconnected();
                if (this.x_debug()) {
                    finish(error);
                    return;
                }
                this.schedule_lsp_reconnect(generation);
                finish();
            };
            const socket = new net.Socket();
            this.lsp_socket = socket;
            socket.on('error', (error) => {
                console.log("RUST socket error");
                console.log(error);
                console.log("RUST /error");
            });
            socket.on('close', () => {
                console.log("RUST socket closed");
                const closedBeforeReady = !settled;
                const action = lspSocketCloseAction({
                    generation,
                    currentGeneration: this.lifecycleGeneration,
                    reconnectGeneration: this.reconnectGeneration,
                    socketIsCurrent: this.lsp_socket === socket,
                    debug: Boolean(this.x_debug()),
                });
                if (action === "ignore") {
                    finish();
                    return;
                }
                this.dispose_lsp_resources(socket);
                this.mark_lsp_disconnected();
                if (action === "reconnect") {
                    this.schedule_lsp_reconnect(generation);
                }
                if (action === "disconnect" && this.x_debug() && closedBeforeReady) {
                    finish(new Error("RUST LSP socket closed before ready"));
                    return;
                }
                finish();
            });
            socket.on('connect', async () => {
                if (connectTimer) {
                    clearTimeout(connectTimer);
                    connectTimer = undefined;
                }
                if (!this.is_current_generation(generation)) {
                    socket.destroy();
                    finish();
                    return;
                }
                console.log("RUST LSP socket connected");
                const client = new lspClient.LanguageClient(
                    'Custom rust LSP server',
                    async () => {
                        if (this.lsp_socket !== socket || !this.is_current_generation(generation)) {
                            return Promise.reject("RUST LSP socket was superseded");
                        }
                        return Promise.resolve({
                            reader: socket,
                            writer: socket,
                        });
                    },
                    this.lsp_client_options
                );
                this.lsp_client = client;
                const disposable = client.start();
                this.lsp_disposable = disposable;
                console.log(`RUST START`);
                try {
                    await this.wait_lsp_client_ready(client);
                    if (!this.is_current_generation(generation) || this.lsp_socket !== socket) {
                        this.dispose_lsp_resources(socket, client, disposable);
                        finish();
                        return;
                    }
                    this.mark_lsp_ready();
                    console.log(`RUST /START`);
                    await this.read_caps();
                    if (!this.is_current_generation(generation)) {
                        finish();
                        return;
                    }
                    if (this.lsp_socket !== socket) {
                        this.mark_lsp_disconnected();
                        finish();
                        return;
                    }
                    await this.fetch_toolbox_config().catch(error => console.log(["fetch_toolbox_config", error]));
                } catch (e) {
                    const error = e instanceof Error ? e : new Error(String(e));
                    console.log(`RUST START PROBLEM e=${error.message}`);
                    this.dispose_lsp_resources(socket, client, disposable);
                    if (!this.is_current_generation(generation)) {
                        finish();
                        return;
                    }
                    this.mark_lsp_disconnected();
                    if (this.x_debug()) {
                        finish(error);
                        return;
                    }
                    this.schedule_lsp_reconnect(generation);
                }
                finish();
            });
            connectTimer = setTimeout(() => {
                if (!settled) {
                    console.log("RUST LSP socket connect timeout");
                    fail_or_reconnect(socket, new Error(`RUST LSP socket connect timed out after ${lspSocketConnectTimeoutMs}ms`));
                }
            }, lspSocketConnectTimeoutMs);
            socket.connect(lspPort, "127.0.0.1");
        });
    }

    async rag_status() {
        try {
            let resp = await this.fetch_project_proxy("/v1/rag-status", {
                method: "GET",
                redirect: "follow",
                cache: "no-cache",
                referrer: "no-referrer",
            }, { timeout: 5000 });
            if (resp.status !== 200) {
                console.log(["rag status http status", resp.status]);
                return Promise.reject("rag status bad status");
            }
            let rag_status = await resp.json();
            return rag_status;
        } catch (e) {
            console.log(["rag status error:", e]);
        }
        return false;
    }

    async fetch_toolbox_config(): Promise<ToolboxConfig> {
        const response = await this.fetch_project_proxy("/v1/customization", { method: "GET" }, { timeout: 5000 });

        if (!response.ok) {
            console.log([
                "fetch_toolbox_config: Error fetching toolbox config",
                response.status,
                this.engine_url("/v1/customization"),
            ]);
            return Promise.reject(
                `Error fetching toolbox config: [status: ${response.status}] [statusText: ${response.statusText}]`
            );
        }

        const json = await response.json() as ToolboxConfig;
        console.log(["success fetch_toolbox_config", json]);

        global.toolbox_config = json;
        await register_commands();
        return json;
    }

    private schedule_lsp_reconnect(generation: number) {
        if (this.reconnectTimer || !this.is_current_generation(generation)) {
            return;
        }
        const delay = Math.min(30000, 1000 * Math.pow(2, this.reconnectAttempts++));
        console.log(`RUST scheduling engine reconnect in ${delay}ms`);
        this.reconnectTimer = setTimeout(() => {
            this.reconnectTimer = undefined;
            if (!this.is_current_generation(generation) || this.reconnectGeneration !== generation) {
                return;
            }
            this.enqueueLifecycle(async (nextGeneration) => this.attach_engine_serialized(nextGeneration));
        }, delay);
    }

    private clearReconnectTimer() {
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = undefined;
        }
    }

    private maybe_register_shared_refact_path(binPath: string): void {
        const homeDir = os.homedir();
        if (!refactBinary.isSharedRefactBinaryPath(binPath, homeDir)) {
            return;
        }
        void registerSharedRefactPath({ homeDir })
            .then(result => {
                if (result.outcome === "warning") {
                    console.warn(`Refact PATH registration skipped: ${result.message ?? "unknown reason"}`);
                }
            })
            .catch(error => {
                console.warn(`Refact PATH registration failed: ${error instanceof Error ? error.message : String(error)}`);
            });
    }

    private plugin_version(): string {
        return vscode.extensions.getExtension("smallcloud.codify")?.packageJSON?.version ?? "0.0.0";
    }

    private warn_engine_version_fallback(pinned: string, chosen: string): void {
        const key = `${pinned}->${chosen}`;
        if (this.warned_engine_fallbacks.has(key)) {
            return;
        }
        this.warned_engine_fallbacks.add(key);
        const message = `Refact engine v${pinned} is not published on GitHub releases; using the latest published engine v${chosen} instead.`;
        console.log(message);
        void vscode.window.showWarningMessage(message);
    }

    private workspace_roots(): string[] {
        return (vscode.workspace.workspaceFolders ?? [])
            .filter(folder => folder.uri.scheme === "file")
            .map(folder => folder.uri.fsPath);
    }

    private engine_settings(): { ast: boolean; astMaxFiles: number; vecdb: boolean; vecdbMaxFiles: number } {
        const config = vscode.workspace.getConfiguration();
        return {
            ast: config.get<boolean>("refactai.ast") ?? true,
            vecdb: config.get<boolean>("refactai.vecdb") ?? true,
            astMaxFiles: config.get<number>("refactai.astFileLimit") ?? 35000,
            vecdbMaxFiles: config.get<number>("refactai.vecdbFileLimit") ?? 15000,
        };
    }
}

export type ChatMessageFromLsp = {
    role: string;
    content: string;
};

export type ToolboxCommand = {
    description: string;
    messages: ChatMessageFromLsp[];
    selection_needed: number[];
    selection_unwanted: boolean;
    insert_at_cursor: boolean;
};

export type SystemPrompt = {
    description: string;
    text: string;
};

export type ToolboxConfig = {
    system_prompts: Record<string, SystemPrompt>;
    toolbox_commands: Record<string, ToolboxCommand>;
};
