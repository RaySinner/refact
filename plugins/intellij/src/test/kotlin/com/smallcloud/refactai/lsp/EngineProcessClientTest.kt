package com.smallcloud.refactai.lsp

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

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
        assertTrue(engineBuildInfoMatches(null, "8.1.0"))
        assertTrue(engineBuildInfoMatches("", "8.1.0"))
        assertTrue(engineBuildInfoMatches("not-json", "8.1.0"))
        assertTrue(engineBuildInfoMatches("""{"version":"8.2.0"}""", "8.1.0"))
        assertTrue(engineBuildInfoMatches("""{"version":"8.1.0"}""", "8.1.0"))
        assertFalse(engineBuildInfoMatches("""{"version":"8.0.0"}""", "8.1.0"))
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
    fun killAdoptedHandleIsNoOp() {
        val fake = RecordingKillClient()
        val handle = EngineHandle(httpPort = 12345, lspPort = 12346, owned = false, pid = 9999)
        fake.kill(handle)
        assertEquals(listOf("shutdown-requested"), fake.calls)
    }

    @Test
    fun killOwnedHandleRequestsGracefulShutdown() {
        val fake = RecordingKillClient()
        val handle = EngineHandle(httpPort = 12345, lspPort = 12346, owned = true, pid = null)
        fake.kill(handle)
        assertEquals(listOf("shutdown-requested"), fake.calls)
    }
}

private class RecordingKillClient : EngineProcessClient() {
    val calls = mutableListOf<String>()

    override fun kill(handle: EngineHandle) {
        calls.add("shutdown-requested")
    }
}
