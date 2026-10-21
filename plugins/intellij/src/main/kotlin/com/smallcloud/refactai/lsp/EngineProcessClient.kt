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
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

const val ENGINE_READY_TIMEOUT_SECONDS = 120L
const val ENGINE_READY_POLL_INTERVAL_MS = 250L
const val ENGINE_SPAWN_MAX_ATTEMPTS = 3
const val ENGINE_BUSY_EXIT_CODES_FILED = 75
const val ENGINE_SHUTDOWN_REQUEST_TIMEOUT_SECONDS = 5L
const val ENGINE_SHUTDOWN_DESTROY_TIMEOUT_SECONDS = 10L
private const val ENGINE_HTTP_GET_TIMEOUT_MS = 5_000
private const val ENGINE_PROCESS_EXIT_LOG_WAIT_MS = 500L

/**
 * The engine worker process is spawned directly as an attached child of the IDE
 * (no setsid/nohup wrappers, stdin is held open by the IDE so the engine's
 * --watch-parent-stdin watchdog only sees EOF when the IDE dies).
 *
 * Windows console suppression is best-effort and targeted: the child's console window is
 * located by pid (EnumWindows + GetWindowThreadProcessId) and hidden right after spawn.
 * The foreground window is never touched, so the IDE window cannot be hidden by accident.
 * CREATE_NO_WINDOW is not reachable through GeneralCommandLine, and a JNA CreateProcessW
 * wrapper would mean owning stdin/stdout wiring ourselves, so post-spawn hiding is used.
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
        args.add("--no-mdns")
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

internal fun isWindows(): Boolean {
    return System.getProperty("os.name").lowercase().contains("win")
}

internal fun engineProcessAlive(pid: Long?): Boolean {
    if (pid == null || pid <= 0) return false
    return runCatching { ProcessHandle.of(pid).orElse(null)?.isAlive == true }.getOrDefault(false)
}

internal fun engineBuildInfoMatches(buildInfoJson: String?, pluginVersion: String): Boolean {
    val raw = buildInfoJson?.trim().takeUnless { it.isNullOrEmpty() } ?: return false
    val version = runCatching {
        val obj = Gson().fromJson(raw, Map::class.java)
        @Suppress("UNCHECKED_CAST")
        ((obj as? Map<String, Any?>)?.get("version") as? String)?.trim()
    }.getOrNull().orEmpty()
    if (version.isEmpty()) return false
    return !versionIsOlder(version, pluginVersion)
}

internal fun engineAdoptionDecision(
    report: EngineBusyReport,
    lastAdoptedPid: Long?,
    requiredVersion: String,
    buildInfoFor: (EngineHandle) -> String?,
): EngineSpawnAttemptResult {
    val pid = report.holderPid ?: return EngineSpawnAttemptResult.RETRY
    val httpPort = report.holderHttpPort ?: return EngineSpawnAttemptResult.RETRY
    if (!engineProcessAlive(pid)) return EngineSpawnAttemptResult.RETRY
    if (pid == lastAdoptedPid) return EngineSpawnAttemptResult.FAIL
    val handle = EngineHandle(httpPort, report.holderLspPort ?: 0, owned = false, pid = pid)
    return if (engineBuildInfoMatches(buildInfoFor(handle), requiredVersion)) {
        EngineSpawnAttemptResult.ADOPT
    } else {
        EngineSpawnAttemptResult.FAIL
    }
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

/**
 * Hides only the console window owned by [process]; other windows (including whatever
 * window currently has focus) are left alone. See the class-level comment for why this is
 * done after spawn instead of through CREATE_NO_WINDOW. JNA is used reflectively because
 * the plugin does not compile against it; when it is unavailable the engine still runs.
 */
internal fun hideWindowsConsoleWindow(process: Process) {
    if (!isWindows()) return
    val pid = process.pid()
    runCatching {
        val user32Class = Class.forName("com.sun.jna.platform.win32.User32")
        val user32 = Class.forName("com.sun.jna.Native").getMethod(
            "load",
            String::class.java,
            Class::class.java,
        ).invoke(null, "user32", user32Class)
        val hwndClass = Class.forName("com.sun.jna.platform.win32.WinDef\$HWND")
        val lparamClass = Class.forName("com.sun.jna.platform.win32.WinDef\$LPARAM")
        val intByReferenceClass = Class.forName("com.sun.jna.ptr.IntByReference")
        val enumProcClass = Class.forName("com.sun.jna.platform.win32.WinUser\$WNDENUMPROC")
        val enumWindows = user32Class.getMethod("EnumWindows", enumProcClass, lparamClass)
        val getWindowThreadProcessId =
            user32Class.getMethod("GetWindowThreadProcessId", hwndClass, intByReferenceClass)
        val showWindow = user32Class.getMethod("ShowWindow", hwndClass, java.lang.Integer.TYPE)
        val getValue = intByReferenceClass.getMethod("getValue")

        val childWindows = mutableListOf<Any>()
        val callback = java.lang.reflect.Proxy.newProxyInstance(
            enumProcClass.classLoader,
            arrayOf(enumProcClass),
        ) { proxy, method, args ->
            when (method.name) {
                "callback" -> {
                    val hwnd = args!![0]
                    val ownerPid = intByReferenceClass.getConstructor().newInstance()
                    getWindowThreadProcessId.invoke(user32, hwnd, ownerPid)
                    if ((getValue.invoke(ownerPid) as Int).toLong() == pid) {
                        childWindows.add(hwnd)
                    }
                    true
                }
                "equals" -> proxy === args?.getOrNull(0)
                "hashCode" -> System.identityHashCode(proxy)
                "toString" -> "refact-engine-window-filter"
                else -> null
            }
        }
        val noParam = lparamClass.getConstructor(java.lang.Long.TYPE).newInstance(0L)
        enumWindows.invoke(user32, callback, noParam)
        childWindows.forEach { showWindow.invoke(user32, it, 0) }
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
    private val shutdownDestroyTimeoutSeconds: Long = ENGINE_SHUTDOWN_DESTROY_TIMEOUT_SECONDS,
) {
    private val logger = Logger.getInstance(EngineProcessClient::class.java)
    internal val engineProcesses = ConcurrentHashMap<Int, Process>()

    fun workerLogPath(root: String): String = logPathProvider(root)

    open fun spawn(
        binPath: String,
        root: String,
        projectId: String,
        settings: LSPConfig,
        pingMessage: String,
        onBusy: (EngineBusyReport) -> EngineSpawnAttemptResult,
    ): EngineHandle {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(ENGINE_READY_TIMEOUT_SECONDS)
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
                    httpPort, lspPort, deadline,
                )
            } catch (e: EngineSpawnFailedException) {
                if (e.exitCode != null && enginePortBusyExitCode(e.exitCode)) {
                    logger.info("Engine port conflict (exit ${e.exitCode}) on attempt $attempt; retrying")
                    return@repeat
                }
                val busy = e.busyReport
                if (busy != null) {
                    when (onBusy(busy)) {
                        EngineSpawnAttemptResult.ADOPT -> {
                            val pid = busy.holderPid
                            val http = busy.holderHttpPort
                            if (pid != null && http != null) {
                                return EngineHandle(
                                    httpPort = http,
                                    lspPort = busy.holderLspPort ?: 0,
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
                            lastBusy = busy
                            return@repeat
                        }
                        EngineSpawnAttemptResult.FAIL -> {
                            throw EngineSpawnFailedException(
                                e.exitCode,
                                e.outputTail,
                                "incompatible existing holder pid=${busy.holderPid} " +
                                    "http_port=${busy.holderHttpPort} for $root",
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
        deadline: Long,
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
        engineProcesses[httpPort] = process
        runCatching { process.outputStream.flush() }

        val outputTail = OutputTailCollector()
        pipeProcessOutput(process, outputTail)

        if (!waitReady(process, httpPort, pingMessage, deadline)) {
            val exitCode = pollExitCode(process)
            val busyLine = outputTail.busyLine()
            val busyReport = busyLine?.let { engineBusyReportFromJson(it) }
            discardProcess(process, httpPort)
            if (busyReport != null) {
                throw EngineSpawnFailedException(
                    exitCode,
                    outputTail.tail(),
                    "workspace busy: $busyLine",
                ).also { it.busyReport = busyReport }
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

    internal fun waitReady(process: Process, httpPort: Int, pingMessage: String, deadline: Long): Boolean {
        while (System.nanoTime() < deadline) {
            if (!process.isAlive) return false
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
        val process = engineProcesses.remove(handle.httpPort)
        runCatching {
            engineHttpPost(engineShutdownUrl(handle.httpPort), "{}")
        }
        if (process != null) {
            if (awaitExit(process)) return
            process.destroy()
            if (awaitExit(process)) return
            process.destroyForcibly()
            if (awaitExit(process)) return
        }
        val pid = handle.pid ?: return
        if (!engineProcessAlive(pid)) return
        if (isWindows()) {
            windowsForceKill(pid)
        } else {
            runCatching { ProcessHandle.of(pid).ifPresent { it.destroyForcibly() } }
        }
    }

    internal fun windowsForceKill(pid: Long) {
        if (!isWindows()) return
        runCatching {
            val process = ProcessBuilder("taskkill", "/T", "/F", "/PID", pid.toString())
                .start()
            process.waitFor(5, TimeUnit.SECONDS)
        }
    }

    private fun awaitExit(process: Process): Boolean {
        return runCatching {
            process.waitFor(shutdownDestroyTimeoutSeconds, TimeUnit.SECONDS)
        }.getOrDefault(false)
    }

    private fun discardProcess(process: Process, httpPort: Int) {
        engineProcesses.remove(httpPort)
        runCatching {
            process.destroy()
            if (!process.waitFor(3, TimeUnit.SECONDS)) {
                process.destroyForcibly()
            }
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
