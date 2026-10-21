@file:OptIn(okhttp3.ExperimentalOkHttpApi::class)

package com.smallcloud.refactai.lsp

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.Protocol
import java.util.concurrent.TimeUnit

class EngineProcessClientTest {

    @Test
    fun allocPortsReturnsTwoDistinctPorts() {
        val ports = EngineProcessClient.allocPorts()
        assertEquals(2, ports?.let { listOf(it.first, it.second) }?.distinct()?.size)
        assertNotEquals(ports?.first, ports?.second)
        assertTrue(ports?.first in 1024..65535)
        assertTrue(ports?.second in 1024..65535)
    }

    @Test
    fun spawnArgsIncludeEngineContractFlags() {
        val args = EngineSpawnArgs(
            binPath = "C:/tools/refact.exe",
            root = "C:/work/project",
            httpPort = 18081,
            lspPort = 18082,
            pingMessage = "ping-nonce",
            projectId = "abc",
            logPath = "C:/logs/engine.log",
            ast = true,
            astMaxFiles = 1234,
            vecdb = true,
            vecdbMaxFiles = 4321,
        ).toList()

        assertTrue(args.contains("worker"))
        assertTrue(args.contains("--workspace-folder"))
        assertTrue(args.contains("C:/work/project"))
        assertTrue(args.contains("--http-port"))
        assertTrue(args.contains("18081"))
        assertTrue(args.contains("--http-host"))
        assertTrue(args.contains("127.0.0.1"))
        assertTrue(args.contains("--lsp-port"))
        assertTrue(args.contains("18082"))
        assertTrue(args.contains("--ping-message"))
        assertTrue(args.contains("ping-nonce"))
        assertTrue(args.contains("--project-id"))
        assertTrue(args.contains("abc"))
        assertTrue(args.contains("--logs-to-file"))
        assertTrue(args.contains("C:/logs/engine.log"))
        assertTrue(args.contains("--ast"))
        assertTrue(args.contains("--ast-max-files"))
        assertTrue(args.contains("1234"))
        assertTrue(args.contains("--vecdb"))
        assertTrue(args.contains("--vecdb-max-files"))
        assertTrue(args.contains("4321"))
        assertTrue(args.contains("--watch-parent-stdin"))
        assertTrue(args.contains("--no-mdns"))
        assertFalse(args.contains("setsid"))
        assertFalse(args.contains("nohup"))
    }

    @Test
    fun spawnArgsOmitFlagsWhenFeaturesDisabled() {
        val args = EngineSpawnArgs(
            binPath = "refact",
            root = "/tmp",
            httpPort = 1,
            lspPort = 2,
            pingMessage = "n",
            projectId = "p",
            logPath = "l",
            ast = false,
            astMaxFiles = null,
            vecdb = false,
            vecdbMaxFiles = null,
        ).toList()

        assertFalse(args.contains("--ast"))
        assertFalse(args.contains("--ast-max-files"))
        assertFalse(args.contains("--vecdb"))
        assertFalse(args.contains("--vecdb-max-files"))
        assertTrue(args.contains("--watch-parent-stdin"))
    }

    @Test
    fun spawnArgsUseDefaultLimitsWhenNull() {
        val args = EngineSpawnArgs(
            binPath = "refact",
            root = "/tmp",
            httpPort = 1,
            lspPort = 2,
            pingMessage = "n",
            projectId = "p",
            logPath = "l",
            ast = true,
            astMaxFiles = null,
            vecdb = true,
            vecdbMaxFiles = null,
        ).toList()

        assertTrue(args.contains(LSPConfig.DEFAULT_AST_MAX_FILES.toString()))
        assertTrue(args.contains(LSPConfig.DEFAULT_VECDB_MAX_FILES.toString()))
    }

    @Test
    fun parsePortsFromArgsReadsFlagValues() {
        val ports = parsePortsFromArgs(
            listOf("refact", "worker", "--http-port", "1234", "--lsp-port", "5678"),
        )
        assertEquals(1234, ports?.first)
        assertEquals(5678, ports?.second)
    }

    @Test
    fun parsePortsFromArgsReturnsNullWhenMissing() {
        assertNull(parsePortsFromArgs(listOf("refact", "worker")))
        assertNull(parsePortsFromArgs(listOf("refact", "worker", "--http-port", "1")))
        assertNull(parsePortsFromArgs(listOf("refact", "worker", "--lsp-port", "2")))
    }

    @Test
    fun busyReportParsesHolderFields() {
        val report = engineBusyReportFromJson(
            """{"workspace_busy":true,"holder_pid":42,"holder_http_port":8001,"holder_lsp_port":8002,"root":"/tmp/x"}""",
        )
        assertEquals(true, report?.workspaceBusy)
        assertEquals(42L, report?.holderPid)
        assertEquals(8001, report?.holderHttpPort)
        assertEquals(8002, report?.holderLspPort)
        assertEquals("/tmp/x", report?.root)
    }

    @Test
    fun busyReportParsesNullHolderFields() {
        val report = engineBusyReportFromJson(
            """{"workspace_busy":true,"holder_pid":null,"holder_http_port":null,"holder_lsp_port":null,"root":"/tmp/y"}""",
        )
        assertEquals(true, report?.workspaceBusy)
        assertNull(report?.holderPid)
        assertNull(report?.holderHttpPort)
        assertNull(report?.holderLspPort)
    }

    @Test
    fun busyReportRejectsOtherJson() {
        assertNull(engineBusyReportFromJson(""))
        assertNull(engineBusyReportFromJson("not json"))
        assertNull(engineBusyReportFromJson("""{"workspace_busy":false,"root":"/tmp"}"""))
    }

    @Test
    fun buildInfoMatcherAcceptsCompatibleVersions() {
        assertTrue(engineBuildInfoMatches("""{"version":"8.2.0"}""", "8.1.0"))
        assertTrue(engineBuildInfoMatches("""{"version":"8.1.0"}""", "8.1.0"))
        assertFalse(engineBuildInfoMatches("""{"version":"8.0.0"}""", "8.1.0"))
    }

    @Test
    fun buildInfoMatcherFailsClosedOnUnverifiableHolder() {
        assertFalse(engineBuildInfoMatches(null, "8.1.0"))
        assertFalse(engineBuildInfoMatches("", "8.1.0"))
        assertFalse(engineBuildInfoMatches("   ", "8.1.0"))
        assertFalse(engineBuildInfoMatches("not-json", "8.1.0"))
        assertFalse(engineBuildInfoMatches("""{"version":""}""", "8.1.0"))
        assertFalse(engineBuildInfoMatches("""{"version":null}""", "8.1.0"))
        assertFalse(engineBuildInfoMatches("""{"build":"abc"}""", "8.1.0"))
    }

    @Test
    fun adoptionDecisionRetriesWithoutHolderIdentity() {
        val noPid = EngineBusyReport(true, null, 8001, 8002, "/tmp")
        assertEquals(EngineSpawnAttemptResult.RETRY, engineAdoptionDecision(noPid, null, "8.1.0") { null })

        val noPort = EngineBusyReport(true, ProcessHandle.current().pid(), null, null, "/tmp")
        assertEquals(EngineSpawnAttemptResult.RETRY, engineAdoptionDecision(noPort, null, "8.1.0") { null })
    }

    @Test
    fun adoptionDecisionAdoptsCompatibleHolder() {
        val report = EngineBusyReport(true, ProcessHandle.current().pid(), 8001, 8002, "/tmp")
        val result = engineAdoptionDecision(report, null, "8.1.0") { handle ->
            assertEquals(8001, handle.httpPort)
            assertEquals(8002, handle.lspPort)
            assertFalse(handle.owned)
            """{"version":"8.1.0"}"""
        }
        assertEquals(EngineSpawnAttemptResult.ADOPT, result)
    }

    @Test
    fun adoptionDecisionFailsClosedOnMissingOrOldBuildInfo() {
        val report = EngineBusyReport(true, ProcessHandle.current().pid(), 8001, 8002, "/tmp")
        assertEquals(EngineSpawnAttemptResult.FAIL, engineAdoptionDecision(report, null, "8.1.0") { null })
        assertEquals(EngineSpawnAttemptResult.FAIL, engineAdoptionDecision(report, null, "8.1.0") { "" })
        assertEquals(
            EngineSpawnAttemptResult.FAIL,
            engineAdoptionDecision(report, null, "8.1.0") { """{"version":"8.0.0"}""" },
        )
    }

    @Test
    fun adoptionDecisionRefusesToReAdoptSameHolder() {
        val pid = ProcessHandle.current().pid()
        val report = EngineBusyReport(true, pid, 8001, 8002, "/tmp")
        assertEquals(
            EngineSpawnAttemptResult.FAIL,
            engineAdoptionDecision(report, pid, "8.1.0") { """{"version":"8.1.0"}""" },
        )
    }

    @Test
    fun exitCodeClassifiers() {
        assertTrue(engineBusyExitCode(75))
        assertFalse(engineBusyExitCode(0))
        assertTrue(enginePortBusyExitCode(48))
        assertTrue(enginePortBusyExitCode(98))
        assertTrue(enginePortBusyExitCode(10048))
        assertFalse(enginePortBusyExitCode(75))
    }

    @Test
    fun engineUrlsUseGivenPorts() {
        assertEquals("http://127.0.0.1:18081/v1/ping", enginePingUrl(18081).toString())
        assertEquals("http://127.0.0.1:18081/v1/build_info", engineBuildInfoUrl(18081).toString())
        assertEquals("http://127.0.0.1:18081/v1/graceful-shutdown", engineShutdownUrl(18081).toString())
    }

    @Test
    fun killAdoptedHandleSendsNoShutdownRequestAndKeepsProcess() {
        withShutdownServer { server ->
            val process = startSleeper()
            val client = EngineProcessClient(shutdownDestroyTimeoutSeconds = 0)
            client.engineProcesses[server.port] = process
            try {
                client.kill(EngineHandle(httpPort = server.port, lspPort = 0, owned = false, pid = process.pid()))

                assertEquals(0, server.requestCount)
                assertTrue("adopted engine must not be killed", process.isAlive)
                assertTrue(client.engineProcesses.containsKey(server.port))
            } finally {
                process.destroyForcibly()
            }
        }
    }

    @Test
    fun killOwnedHandlePostsGracefulShutdownThenKillsProcess() {
        withShutdownServer { server ->
            server.enqueue(MockResponse.Builder().code(200).body("{}").build())
            val process = startSleeper()
            val client = EngineProcessClient(shutdownDestroyTimeoutSeconds = 0)
            client.engineProcesses[server.port] = process
            try {
                client.kill(EngineHandle(httpPort = server.port, lspPort = 0, owned = true, pid = process.pid()))

                val request = server.takeRequest()
                assertEquals("POST", request.method)
                assertEquals("/v1/graceful-shutdown", request.path)
                assertEquals(1, server.requestCount)
                assertTrue("engine process must be killed", process.waitFor(10, TimeUnit.SECONDS))
                assertFalse(process.isAlive)
                assertFalse(client.engineProcesses.containsKey(server.port))
            } finally {
                process.destroyForcibly()
            }
        }
    }

    @Test
    fun waitReadyFailsFastWhenChildExited() {
        val process = startSleeper()
        process.destroyForcibly()
        assertTrue(process.waitFor(10, TimeUnit.SECONDS))
        val client = EngineProcessClient()
        val startedAt = System.nanoTime()

        val ready = client.waitReady(
            process,
            1,
            "never",
            System.nanoTime() + TimeUnit.SECONDS.toNanos(ENGINE_READY_TIMEOUT_SECONDS),
        )

        assertFalse(ready)
        assertTrue(TimeUnit.NANOSECONDS.toSeconds(System.nanoTime() - startedAt) < 5)
    }

    private fun startSleeper(): Process {
        val command = if (System.getProperty("os.name").lowercase().contains("win")) {
            listOf("cmd", "/c", "ping", "-n", "60", "127.0.0.1")
        } else {
            listOf("sleep", "60")
        }
        return ProcessBuilder(command).redirectErrorStream(true).start()
    }

    private fun withShutdownServer(block: (MockWebServer) -> Unit) {
        val server = MockWebServer()
        server.protocols = listOf(Protocol.HTTP_1_1)
        server.start()
        try {
            block(server)
        } finally {
            server.shutdown()
        }
    }
}
