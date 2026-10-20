import { backendReadyForStatus, type RefactBackendConnectionStatus } from "./backendStatus";
import { configuredBrowserHost } from "./engineProcess";

export const lspSocketConnectTimeoutMs = 10000;
export const lspClientReadyTimeoutMs = 20000;
export const lspClientStopTimeoutMs = 5000;

export type LspSocketCloseAction = "ignore" | "reconnect" | "disconnect";

export function shouldRunLifecycleGeneration(generation: number, currentGeneration: number): boolean {
    return generation === currentGeneration;
}

export function lspSocketCloseAction(params: {
    generation: number;
    currentGeneration: number;
    reconnectGeneration: number | undefined;
    socketIsCurrent: boolean;
    debug: boolean;
}): LspSocketCloseAction {
    if (!params.socketIsCurrent || !shouldRunLifecycleGeneration(params.generation, params.currentGeneration)) {
        return "ignore";
    }
    if (!params.debug && params.reconnectGeneration === params.generation) {
        return "reconnect";
    }
    return "disconnect";
}

export function attachStateForEngineLaunch(): RefactBackendConnectionStatus {
    return "starting";
}

export function browserUrlForBackendStatus(params: {
    status: RefactBackendConnectionStatus;
    debug: boolean;
    debugHttpPort: number;
    port: number;
    configuredHost?: string;
}): string {
    if (!backendReadyForStatus(params.status)) {
        return "";
    }
    const host = configuredBrowserHost(params.configuredHost) ?? "127.0.0.1";
    if (params.debug) {
        return `http://${host}:${params.debugHttpPort}/`;
    }
    if (!params.port) {
        return "";
    }
    return `http://${host}:${params.port}/`;
}
