import * as assert from "assert";
import * as os from "os";
import * as path from "path";
import {
    compareVersions,
    configuredBrowserHost,
    selectPrimaryWorkspaceRoot,
} from "./engineProcess";
import {
    isSharedRefactBinaryPath,
    sharedRefactBinaryPath,
    sharedRefactBinaryDir,
} from "./refactBinaryResolver";
import {
    backendConfigForStatus,
    effectiveLspPortForStatus,
    shouldReadCapsForCompletion,
} from "./backendStatus";
import {
    attachStateForDaemonOpenProject,
    browserUrlForBackendStatus,
    lspSocketCloseAction,
    shouldRunLifecycleGeneration,
} from "./launchRustLifecycle";

export async function runRefactDaemonTests() {
    assert.strictEqual(configuredBrowserHost(undefined), undefined);
    assert.strictEqual(configuredBrowserHost(""), undefined);
    assert.strictEqual(configuredBrowserHost(" 0.0.0.0 "), undefined);
    assert.strictEqual(configuredBrowserHost(" machine.local "), "machine.local");

    assert.deepStrictEqual(selectPrimaryWorkspaceRoot(["/repo"]), { primary: "/repo", ignored: [] });
    assert.deepStrictEqual(selectPrimaryWorkspaceRoot([]), { primary: undefined, ignored: [] });
    const multiRootSelection = selectPrimaryWorkspaceRoot(["/repo", "/other", "/third"]);
    assert.strictEqual(multiRootSelection.primary, "/repo");
    assert.deepStrictEqual(multiRootSelection.ignored, ["/other", "/third"]);
    assert.strictEqual(multiRootSelection.warning?.includes("only the primary VS Code workspace folder"), true);

    assert.strictEqual(compareVersions("8.2.0", "8.1.9"), 1);
    assert.strictEqual(compareVersions("8.1.0", "8.1.0-alpha.1"), 1);
    assert.strictEqual(compareVersions("8.1.0-alpha.2", "8.1.0-alpha.10"), -1);
    assert.strictEqual(compareVersions("8.1.0-alpha.1", "8.1.0-beta.1"), -1);
    assert.strictEqual(compareVersions("8.1.0", "8.1.1"), -1);
    assert.strictEqual(compareVersions("8.10", "8.2"), 1);
    assert.strictEqual(compareVersions("8", "8.0.0"), 0);
    assert.strictEqual(compareVersions("8.1", "8.1.0"), 0);
    assert.strictEqual(compareVersions("8.1.1", "8.1"), 1);
    assert.strictEqual(compareVersions("8.1.0-main-4053-d8e6abb9", "8.1.0"), -1);
    assert.strictEqual(compareVersions("8.1.0", "8.1.0-main-4053-d8e6abb9"), 1);
    assert.strictEqual(compareVersions("refact 8.1.0", "8.1.0"), 0);
    assert.strictEqual(compareVersions("8.1.x", "8.1"), 0);
    assert.strictEqual(compareVersions("unparseable", "0.0.0"), 0);
    assert.strictEqual(compareVersions("v8.1.0+build.1", "8.1.0"), 0);

    runBackendStatusTests();
    runLaunchRustLifecycleTests();
    runSharedRefactPathMatchingTests();
}

function runBackendStatusTests() {
    assert.deepStrictEqual(backendConfigForStatus("connecting"), {
        backendReady: false,
        connectionStatus: "connecting",
    });
    assert.deepStrictEqual(backendConfigForStatus("ready"), {
        backendReady: true,
        connectionStatus: "ready",
    });
    assert.strictEqual(effectiveLspPortForStatus(8001, "connecting"), 0);
    assert.strictEqual(effectiveLspPortForStatus(8001, "ready"), 8001);
    assert.strictEqual(shouldReadCapsForCompletion(false, "connecting"), false);
    assert.strictEqual(shouldReadCapsForCompletion(false, "ready"), true);
    assert.strictEqual(shouldReadCapsForCompletion(true, "ready"), false);
}

function runLaunchRustLifecycleTests() {
    assert.strictEqual(attachStateForDaemonOpenProject(), "starting");
    assert.strictEqual(browserUrlForBackendStatus({
        status: "connecting",
        debug: false,
        debugHttpPort: 8001,
        port: 8488,
        configuredHost: "machine.local",
    }), "");
    assert.strictEqual(browserUrlForBackendStatus({
        status: "starting",
        debug: true,
        debugHttpPort: 8001,
        port: 0,
        configuredHost: "machine.local",
    }), "");
    assert.strictEqual(browserUrlForBackendStatus({
        status: "ready",
        debug: true,
        debugHttpPort: 8001,
        port: 0,
        configuredHost: "machine.local",
    }), "http://machine.local:8001/");
    assert.strictEqual(browserUrlForBackendStatus({
        status: "ready",
        debug: false,
        debugHttpPort: 8001,
        port: 8488,
        configuredHost: "machine.local",
    }), "http://machine.local:8488/");
    assert.strictEqual(shouldRunLifecycleGeneration(2, 2), true);
    assert.strictEqual(shouldRunLifecycleGeneration(1, 2), false);
    assert.strictEqual(lspSocketCloseAction({
        generation: 1,
        currentGeneration: 2,
        reconnectGeneration: 1,
        socketIsCurrent: true,
        debug: false,
    }), "ignore");
    assert.strictEqual(lspSocketCloseAction({
        generation: 2,
        currentGeneration: 2,
        reconnectGeneration: 2,
        socketIsCurrent: false,
        debug: false,
    }), "ignore");
    assert.strictEqual(lspSocketCloseAction({
        generation: 2,
        currentGeneration: 2,
        reconnectGeneration: 2,
        socketIsCurrent: true,
        debug: false,
    }), "reconnect");
    assert.strictEqual(lspSocketCloseAction({
        generation: 2,
        currentGeneration: 2,
        reconnectGeneration: undefined,
        socketIsCurrent: true,
        debug: false,
    }), "disconnect");
    assert.strictEqual(lspSocketCloseAction({
        generation: 2,
        currentGeneration: 2,
        reconnectGeneration: 2,
        socketIsCurrent: true,
        debug: true,
    }), "disconnect");
}

function runSharedRefactPathMatchingTests() {
    const posixHome = "/home/tester";
    const posixShared = sharedRefactBinaryPath(posixHome, "linux");
    assert.strictEqual(isSharedRefactBinaryPath(posixShared, posixHome, "linux"), true);
    assert.strictEqual(isSharedRefactBinaryPath(`${posixShared} `, posixHome, "linux"), true);
    assert.strictEqual(isSharedRefactBinaryPath(posixShared.toUpperCase(), posixHome, "linux"), false);
    assert.strictEqual(isSharedRefactBinaryPath("/usr/local/bin/refact", posixHome, "linux"), false);
    assert.strictEqual(isSharedRefactBinaryPath(undefined, posixHome, "linux"), false);
    assert.strictEqual(isSharedRefactBinaryPath("", posixHome, "linux"), false);
    assert.strictEqual(sharedRefactBinaryDir(posixHome), path.resolve("/home/tester/.refact/bin"));

    const winHome = "C:\\Users\\Tester";
    const winShared = sharedRefactBinaryPath(winHome, "win32");
    assert.strictEqual(isSharedRefactBinaryPath(winShared, winHome, "win32"), true);
    assert.strictEqual(isSharedRefactBinaryPath(winShared.toLowerCase(), winHome, "win32"), true);
    assert.strictEqual(isSharedRefactBinaryPath(winShared.toUpperCase(), winHome, "win32"), true);
    assert.strictEqual(isSharedRefactBinaryPath("C:\\other\\refact.exe", winHome, "win32"), false);
}

runRefactDaemonTests().catch(error => {
    console.error(error);
    process.exitCode = 1;
});
