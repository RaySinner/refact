package com.smallcloud.refactai.lsp

import com.google.gson.Gson
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.openapi.diagnostic.Logger
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.ServerSocket
import java.net.URI
import java.nio.charset.StandardCharsets
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

const val ENGINE_READY_TIMEOUT_SECONDS = 120L
const val ENGINE_READY_POLL_INTERVAL_MS = 250L
const val ENGINE_SPAWN_MAX_ATTEMPTS = 3
const val ENGINE_BUSY_EXIT_CODES_FILED = 75
const val ENGINE_SHUTDOWN_REQUEST_TIMEOUT_SECONDS = 5L
const val ENGINE_SHUTDOWN_DESTROY_TIMEOUT_SECONDS = 10L
const val ENGINE_LSP_PING_MESSAGE = "refact-lsp-ping"
const val ENGINE_DEFAULT_WORKER_HTTP_PORT = 8001
const val ENGINE_DEFAULT_WORKER_LSP_PORT = 8002
private const val ENGINE_HTTP_GET_TIMEOUT_MS = 5_000
private const val ENGINE_PROCESS_EXIT_LOG_WAIT_MS = 500L

/**
 * The engine worker process is spawned directly as an attached child of the IDE
 * (no setsid/nohup wrappers, stdin is held open by the IDE so the engine's
 * --watch-parent-stdin watchdog only sees EOF when the IDE dies).
 *
 * Windows console suppression: JVM child processes started through
 * ProcessBuilder/GeneralCommandLine do not get a new console window when the
 * launching JVM itself is windowed (the IDE), because no conhost is attached
 * unless the child was linked as a console subsystem binary that requires one.
 * The `refact` engine is a console-subsystem binary, so on Windows we hide the
 * console window of the freshly created child via JNA (CreateProcessW with
 * CREATE_NO_WINDOW is not reachable through GeneralCommandLine, but the window
 * can be found and hidden right after spawn). This is best-effort: if JNA is
 * unavailable the child is still functional, only the console flash remains.
 */
data class EngineHandle(
    val httpPort: Int,
    val lspPort: Int,
    val owned: Boolean,
    val pid: Long?,
)

data class EngineSpawnArgs(
    val binPath: String,
    val root: String,
    val httpPort: Int,
    val lspPort: Int,
    val pingMessage: String,
    val projectId: String,
    val logPath: String,
    val ast: Boolean,
    val astMaxFiles: Int?,
    val vecdb: Boolean,
    val vecdbMaxFiles: Int?,
    val watchParentStdin: Boolean = true,
) {
    fun toList(): List<String> {
        val args = mutableListOf(
            binPath,
            "worker",
            "--workspace-folder",
            root,
            "--http-port",
            httpPort.toString(),
            "--http-host",
            "127.0.0.1",
            "--lsp-port",
            lspPort.toString(),
            "--ping-message",
            pingMessage,
            "--project-id",
            projectId,
            "--logs-to-file",
            logPath,
        )
        if (ast) {
            args.add("--ast")
            args.add("--ast-max-files")
            args.add((astMaxFiles ?: LSPConfig.DEFAULT_AST_MAX_FILES).toString())
        }
        if (vecdb) {
            args.add("--vecdb")
            args.add("--vecdb-max-files")
            args.add((vecdbMaxFiles ?: LSPConfig.DEFAULT_VECDB_MAX_FILES).toString())
        }
        if (watchParentStdin) {
            args.add("--watch-parent-stdin")
        }
        return args
    }
}

data class EngineBusyReport(
    val workspaceBusy: Boolean,
    val holderPid: Long?,
    val holderHttpPort: Int?,
    val holderLspPort: Int?,
    val root: String,
)

fun parsePortsFromArgs(argv: List<String>): Pair<Int, Int>? {
    fun valueAfter(flag: String): Int? {
        val index = argv.indexOf(flag)
        if (index < 0 || index + 1 >= argv.size) return null
        return argv[index + 1].toIntOrNull()
    }
    val http = valueAfter("--http-port") ?: return null
    val lsp = valueAfter("--lsp-port") ?: return null
    return http to lsp
}

fun buildEngineWorkerArgs(settings: LSPConfig): Map<String, Any> {
    return mapOf(
        "ast" to settings.ast,
        "ast_max_files" to (settings.astFileLimit ?: LSPConfig.DEFAULT_AST_MAX_FILES),
        "vecdb" to settings.vecdb,
        "vecdb_max_files" to (settings.vecdbFileLimit ?: LSPConfig.DEFAULT_VECDB_MAX_FILES),
        "codegraph" to settings.codegraph,
        "codegraph_max_files" to (settings.codegraphFileLimit ?: LSPConfig.DEFAULT_CODEGRAPH_MAX_FILES),
    )
}

fun engineBusyReportFromJson(json: String): EngineBusyReport? {
    if (json.isBlank()) return null
    val obj = runCatching { Gson().fromJson(json, Map::class.java) }.getOrNull() ?: return null
    @Suppress("UNCHECKED_CAST")
    val map = obj as? Map<String, Any?> ?: return null
    if (map["workspace_busy"] != java.lang.Boolean.TRUE) return null
    return EngineBusyReport(
        workspaceBusy = true,
        holderPid = (map["holder_pid"] as? Number)?.toLong(),
        holderHttpPort = (map["holder_http_port"] as? Number)?.toInt(),
        holderLspPort = (map["holder_lsp_port"] as? Number)?.toInt(),
        root = map["root"] as? String ?: "",
    )
}

internal fun engineProcessAlive(pid: Long?): Boolean {
    if (pid == null || pid <= 0) return false
    return runCatching { ProcessHandle.of(pid).orElse(null)?.isAlive == true }.getOrDefault(false)
}

internal fun engineBuildInfoMatches(buildInfoJson: String?, pluginVersion: String): Boolean {
    val raw = buildInfoJson?.trim().takeUnless { it.isNullOrEmpty() } ?: return true
    val version = runCatching {
        val obj = Gson().fromJson(raw, Map::class.java)
        @Suppress("UNCHECKED_CAST")
        (obj as? Map<String, Any?>)?.get("version") as? String
    }.getOrNull()?.trim().orEmpty()
    if (version.isEmpty()) return true
    return !versionIsOlder(version, pluginVersion)
}

internal fun engineBusyExitCode(exitCode: Int): Boolean {
    return exitCode == ENGINE_BUSY_EXIT_CODES_FILED
}

internal fun enginePortBusyExitCode(exitCode: Int): Boolean {
    return exitCode == 48 || exitCode == 98 || exitCode == 10048
}

internal fun engineShutdownUrl(httpPort: Int): URI {
    return URI("http://127.0.0.1:$httpPort/v1/graceful-shutdown")
}

internal fun engineBuildInfoUrl(httpPort: Int): URI {
    return URI("http://127.0.0.1:$httpPort/v1/build_info")
}

internal fun enginePingUrl(httpPort: Int): URI {
    return URI("http://127.0.0.1:$httpPort/v1/ping")
}

internal fun engineHttpBody(url: URI, timeoutMs: Int = ENGINE_HTTP_GET_TIMEOUT_MS): String? {
    return runCatching {
        val connection = url.toURL().openConnection() as HttpURLConnection
        try {
            connection.requestMethod = "GET"
            connection.connectTimeout = timeoutMs
            connection.readTimeout = timeoutMs
            val statusCode = connection.responseCode
            if (statusCode !in 200..299) return@runCatching null
            connection.inputStream.use { it.readBytes().toString(StandardCharsets.UTF_8) }
        } finally {
            connection.disconnect()
        }
    }.getOrNull()
}

internal fun hideWindowsConsoleWindow(process: Process) {
    if (!System.getProperty("os.name").lowercase().contains("win")) return
    runCatching {
        val windowClass = Class.forName("com.sun.jna.platform.win32.User32")
        val jnaNative = Class.forName("com.sun.jna.Native").getMethod(
            "load",
            String::class.java,
            Class::class.java,
        )
        val user32 = jnaNative.invoke(null, "user32", windowClass)
        val getForegroundWindow = windowClass.methods.first { it.name == "GetForegroundWindow" }
        val hwnd = getForegroundWindow.invoke(user32)
        if (hwnd != null) {
            val showWindow = windowClass.methods.first { it.name == "ShowWindow" }
            showWindow.invoke(user32, hwnd, 0)
        }
    }
}

class EngineSpawnFailedException(
    val exitCode: Int?,
    val outputTail: String,
    message: String,
) : IOException(message) {
    var busyReport: EngineBusyReport? = null
}

open class EngineProcessClient(
    private val logPathProvider: (String) -> String = { root ->
        val dir = File(System.getProperty("user.home"), ".cache/refact/ide")
        dir.mkdirs()
        val safeRoot = root.hashCode().toUInt().toString()
        File(dir, "engine-$safeRoot.log").absolutePath
    },
) {
    private val logger = Logger.getInstance(EngineProcessClient::class.java)

    fun workerLogPath(root: String): String = logPathProvider(root)

    open fun spawn(
        binPath: String,
        root: String,
        projectId: String,
        settings: LSPConfig,
        pingMessage: String,
        onBusy: (EngineBusyReport) -> EngineSpawnAttemptResult,
    ): EngineHandle {
        var lastBusy: EngineBusyReport? = null
        var lastError: Exception? = null
        repeat(ENGINE_SPAWN_MAX_ATTEMPTS) { attempt ->
            val ports = allocPorts()
            if (ports == null) {
                lastError = IOException("failed to allocate two distinct ephemeral ports")
                return@repeat
            }
            val (httpPort, lspPort) = ports
            try {
                return spawnOnce(
                    binPath, root, projectId, settings, pingMessage,
                    httpPort, lspPort, onBusy,
                )
            } catch (e: EngineSpawnFailedException) {
                if (e.exitCode != null && enginePortBusyExitCode(e.exitCode)) {
                    logger.info("Engine port conflict (exit ${e.exitCode}) on attempt $attempt; retrying")
                    return@repeat
                }
                if (e.exitCode != null && engineBusyExitCode(e.exitCode) && e.busyReport != null) {
                    when (onBusy(e.busyReport!!)) {
                        EngineSpawnAttemptResult.ADOPT -> {
                            val report = e.busyReport!!
                            val pid = report.holderPid
                            val http = report.holderHttpPort
                            val lsp = report.holderLspPort
                            if (pid != null && http != null) {
                                return EngineHandle(
                                    httpPort = http,
                                    lspPort = lsp ?: ENGINE_DEFAULT_WORKER_LSP_PORT,
                                    owned = false,
                                    pid = pid,
                                )
                            }
                            lastError = IOException(
                                "existing holder for $root has no pid/http_port: ${e.outputTail.take(200)}",
                            )
                            return@repeat
                        }
                        EngineSpawnAttemptResult.RETRY -> {
                            lastBusy = e.busyReport
                            return@repeat
                        }
                        EngineSpawnAttemptResult.FAIL -> {
                            throw EngineSpawnFailedException(
                                e.exitCode,
                                e.outputTail,
                                "incompatible existing holder pid=${e.busyReport?.holderPid} " +
                                    "http_port=${e.busyReport?.holderHttpPort} for $root",
                            )
                        }
                    }
                }
                throw e
            }
        }
        val busyDetail = lastBusy?.let {
            "busy holder pid=${it.holderPid} http_port=${it.holderHttpPort}"
        } ?: ""
        throw EngineSpawnFailedException(
            null,
            lastError?.message ?: busyDetail,
            "failed to spawn refact engine worker for $root after $ENGINE_SPAWN_MAX_ATTEMPTS attempts. " +
                (lastError?.message ?: busyDetail),
        )
    }

    private fun spawnOnce(
        binPath: String,
        root: String,
        projectId: String,
        settings: LSPConfig,
        pingMessage: String,
        httpPort: Int,
        lspPort: Int,
        onBusy: (EngineBusyReport) -> EngineSpawnAttemptResult,
    ): EngineHandle {
        val logPath = workerLogPath(root)
        val args = EngineSpawnArgs(
            binPath = binPath,
            root = root,
            httpPort = httpPort,
            lspPort = lspPort,
            pingMessage = pingMessage,
            projectId = projectId,
            logPath = logPath,
            ast = settings.ast,
            astMaxFiles = settings.astFileLimit,
            vecdb = settings.vecdb,
            vecdbMaxFiles = settings.vecdbFileLimit,
        ).toList()
        val commandLine = GeneralCommandLine(args).withWorkDirectory(File(root))
        val process = commandLine.createProcess()
        hideWindowsConsoleWindow(process)
        runCatching { process.outputStream.flush() }

        val outputTail = OutputTailCollector()
        pipeProcessOutput(process, outputTail)

        if (!waitReady(httpPort, pingMessage)) {
            val exitCode = pollExitCode(process)
            if (exitCode != null && engineBusyExitCode(exitCode)) {
                val busyLine = outputTail.busyLine()
                val report = busyLine?.let { engineBusyReportFromJson(it) }
                if (report != null) {
                    throw EngineSpawnFailedException(
                        exitCode,
                        outputTail.tail(),
                        "workspace busy: $busyLine",
                    ).also { it.busyReport = report }
                }
            }
            runCatching {
                process.destroy()
                if (!process.waitFor(3, TimeUnit.SECONDS)) {
                    process.destroyForcibly()
                }
            }
            throw EngineSpawnFailedException(
                exitCode,
                outputTail.tail(),
                "engine worker did not become ready on port $httpPort within " +
                    "$ENGINE_READY_TIMEOUT_SECONDS seconds (exit=$exitCode). tail: ${outputTail.tail()}",
            )
        }
        return EngineHandle(httpPort, lspPort, owned = true, pid = process.pid().toLong())
    }

    fun waitReady(httpPort: Int, pingMessage: String): Boolean {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(ENGINE_READY_TIMEOUT_SECONDS)
        while (System.nanoTime() < deadline) {
            val body = engineHttpBody(enginePingUrl(httpPort))
            if (body != null && body.trim() == pingMessage) return true
            sleepPoll()
        }
        return false
    }

    fun buildInfo(handle: EngineHandle): String? {
        return engineHttpBody(engineBuildInfoUrl(handle.httpPort))
    }

    open fun kill(handle: EngineHandle) {
        if (!handle.owned) {
            logger.info("Skip killing adopted engine pid=" + handle.pid + " on port " + handle.httpPort)
            return
        }
        runCatching {
            engineHttpPost(engineShutdownUrl(handle.httpPort), "{}")
        }
        val pid = handle.pid
        if (pid != null && engineProcessAlive(pid)) {
            windowsForceKill(pid)
        }
    }

    internal fun windowsForceKill(pid: Long) {
        if (!System.getProperty("os.name").lowercase().contains("win")) return
        runCatching {
            val process = ProcessBuilder("taskkill", "/T", "/F", "/PID", pid.toString())
                .start()
            process.waitFor(5, TimeUnit.SECONDS)
        }
    }

    private fun pollExitCode(process: Process): Int? {
        return runCatching {
            if (process.waitFor(ENGINE_PROCESS_EXIT_LOG_WAIT_MS, TimeUnit.MILLISECONDS)) {
                process.exitValue()
            } else {
                null
            }
        }.getOrNull()
    }

    private fun sleepPoll() {
        Thread.sleep(ENGINE_READY_POLL_INTERVAL_MS)
    }

    private fun pipeProcessOutput(process: Process, tail: OutputTailCollector) {
        thread(start = true, isDaemon = true, name = "refact-engine-stdout") {
            runCatching {
                process.inputStream.bufferedReader(StandardCharsets.UTF_8).useLines { lines ->
                    lines.forEach { line ->
                        tail.add(line)
                        logger.info("refact engine stdout: $line")
                    }
                }
            }
        }
        thread(start = true, isDaemon = true, name = "refact-engine-stderr") {
            runCatching {
                process.errorStream.bufferedReader(StandardCharsets.UTF_8).useLines { lines ->
                    lines.forEach { line ->
                        tail.add(line)
                        logger.info("refact engine stderr: $line")
                    }
                }
            }
        }
    }

    private class OutputTailCollector {
        private val lines = ArrayDeque<String>()

        @Synchronized
        fun add(line: String) {
            lines.addLast(line)
            while (lines.size > 20) {
                lines.removeFirst()
            }
        }

        @Synchronized
        fun busyLine(): String? {
            return lines.lastOrNull { engineBusyReportFromJson(it) != null }
        }

        @Synchronized
        fun tail(): String {
            return lines.joinToString("\n")
        }
    }

    companion object {
        fun allocPorts(): Pair<Int, Int>? {
            val first = ServerSocket(0)
            val firstPort = runCatching { first.localPort }.getOrNull()
            runCatching { first.close() }
            if (firstPort == null) return null
            val second = ServerSocket(0)
            val secondPort = runCatching { second.localPort }.getOrNull()
            runCatching { second.close() }
            if (secondPort == null) return null
            if (firstPort == secondPort) return null
            return firstPort to secondPort
        }

        fun processIsAlive(handle: EngineHandle): Boolean {
            return engineProcessAlive(handle.pid)
        }
    }
}

enum class EngineSpawnAttemptResult {
    ADOPT,
    RETRY,
    FAIL,
}

internal fun engineHttpPost(url: URI, body: String): String? {
    return runCatching {
        val connection = url.toURL().openConnection() as HttpURLConnection
        try {
            connection.requestMethod = "POST"
            connection.connectTimeout = ENGINE_HTTP_GET_TIMEOUT_MS
            connection.readTimeout = (TimeUnit.SECONDS.toMillis(ENGINE_SHUTDOWN_REQUEST_TIMEOUT_SECONDS)).toInt()
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/json")
            val bytes = body.toByteArray(StandardCharsets.UTF_8)
            connection.setRequestProperty("Content-Length", bytes.size.toString())
            connection.outputStream.use { it.write(bytes) }
            val statusCode = connection.responseCode
            if (statusCode !in 200..299) return@runCatching null
            connection.inputStream.use { it.readBytes().toString(StandardCharsets.UTF_8) }
        } finally {
            connection.disconnect()
        }
    }.getOrNull()
}
