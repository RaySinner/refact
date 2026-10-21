@file:OptIn(okhttp3.ExperimentalOkHttpApi::class)

package com.smallcloud.refactai.lsp

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.serviceContainer.AlreadyDisposedException
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import com.intellij.util.concurrency.AppExecutorUtil
import com.intellij.util.messages.MessageBus
import com.smallcloud.refactai.io.AsyncConnection
import com.smallcloud.refactai.io.HttpStatusException
import com.smallcloud.refactai.panes.sharedchat.Events
import com.smallcloud.refactai.io.InferenceGlobalContext.Companion.instance as InferenceGlobalContext
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.Protocol
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.mockito.Mockito.`when`
import org.mockito.Mockito.mock
import java.net.URI
import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

class LSPProcessHolderTest : BasePlatformTestCase() {

    class FakeEngineClient : EngineProcessClient() {
        val spawnCalls = AtomicInteger(0)
        val killCalls = AtomicInteger(0)
        val killOrders = Collections.synchronizedList(mutableListOf<String>())
        val spawnedRoots = Collections.synchronizedList(mutableListOf<String>())
        val spawnedBinPaths = Collections.synchronizedList(mutableListOf<String>())
        var spawnHandle = EngineHandle(httpPort = 18081, lspPort = 18082, owned = true, pid = null)
        var spawnError: RuntimeException? = null

        override fun spawn(
            binPath: String,
            root: String,
            projectId: String,
            settings: LSPConfig,
            pingMessage: String,
            onBusy: (EngineBusyReport) -> EngineSpawnAttemptResult,
        ): EngineHandle {
            spawnCalls.incrementAndGet()
            spawnedRoots.add(root)
            spawnedBinPaths.add(binPath)
            spawnError?.let { throw it }
            return spawnHandle
        }

        override fun kill(handle: EngineHandle) {
            killCalls.incrementAndGet()
            killOrders.add(if (handle.owned) "owned" else "adopted")
        }
    }

    class TestLspProcessHolder(
        project: Project,
        private val fakeEngineClient: FakeEngineClient,
        private val refreshWorkerState: Boolean = false,
        private val requiredVersion: String = "8.1.0",
    ) : LSPProcessHolder(project) {
        private val latch = CountDownLatch(1)
        val retryAttempts = mutableListOf<Int>()
        val initializeCalls = AtomicInteger(0)
        val initializeEntered = CountDownLatch(1)
        val releaseInitialize = CountDownLatch(1)
        val ensureStartedBlockingEntered = CountDownLatch(1)
        val releaseEnsureStartedBlocking = CountDownLatch(1)
        val binaryResolutionCalls = AtomicInteger(0)
        private val healthClockMs = AtomicLong(0)
        private val binaryResolutionResults = mutableListOf<String?>()
        private val binaryResolutionResultIndex = AtomicInteger(0)
        val binaryResolutionEntered = CountDownLatch(1)
        val releaseBinaryResolution = CountDownLatch(1)
        var blockEnsureStartedBlocking = false
        var blockInitialize = false
        var blockBinaryResolution = false
        var signalDownloadStartDuringResolution = false
        var initializeError: RuntimeException? = null
        var probeResult = true

        val terminalRegistrationPaths = Collections.synchronizedList(mutableListOf<String>())
        var binaryFailureMessage = "Failed to download the Refact engine (version 8.1.0) from GitHub releases"


        override fun refreshAttachedWorkerState() {
            if (refreshWorkerState) super.refreshAttachedWorkerState()
        }

        override fun initializeAttachedProject() {
            initializeCalls.incrementAndGet()
            initializeEntered.countDown()
            if (blockInitialize) {
                releaseInitialize.await(2, TimeUnit.SECONDS)
            }
            initializeError?.let { throw it }
        }

        override fun requiredDaemonVersion(): String {
            return requiredVersion
        }


        override fun registerSharedBinaryPathForTerminal(resolvedPath: String) {
            terminalRegistrationPaths.add(resolvedPath)
        }

        override fun sleepBeforeWakeRetry(attempt: Int) {
            retryAttempts.add(attempt)
        }

        override fun probeAttachedWorker(): Boolean {
            return probeResult
        }

        override fun healthNowMs(): Long {
            return healthClockMs.get()
        }

        override val engineClient: EngineProcessClient
            get() = fakeEngineClient

        override fun attachEngineHandle(root: String, config: LSPConfig) {
            val bin = resolveBinaryPathForDaemon {
                if (signalDownloadStartDuringResolution) {
                    signalDownloadStart()
                }
            } ?: run {
                recordFailureForTest(binaryFailureMessage)
                throw IllegalStateException(binaryFailureMessage)
            }
            clearFailureForTest()
            registerSharedBinaryPathForTerminal(bin)

            val handle = fakeEngineClient.spawn(bin, root, "test", config, "ping") { EngineSpawnAttemptResult.RETRY }
            setEngineHandleForTest(handle)
        }

        override fun resolveBinaryPathForDaemon(onDownloadStart: () -> Unit): String? {
            binaryResolutionCalls.incrementAndGet()
            if (signalDownloadStartDuringResolution) {
                onDownloadStart()
            }
            if (blockBinaryResolution) {
                binaryResolutionEntered.countDown()
                releaseBinaryResolution.await(2, TimeUnit.SECONDS)
            }
            val index = binaryResolutionResultIndex.getAndIncrement()
            return if (index < binaryResolutionResults.size) {
                binaryResolutionResults[index]
            } else {
                LSPProcessHolder.BIN_PATH
            }
        }

        override fun binaryResolutionFailureMessage(): String {
            return binaryFailureMessage
        }

        fun signalDownloadStart() {
            setBackendConnectionStatusPublic(LSPBackendConnectionStatus.INSTALLING)
        }

        fun recordFailureForTest(message: String) {
            recordBinaryResolutionFailureForTest(message)
        }

        fun clearFailureForTest() {
            clearBinaryResolutionFailureForTest()
        }

        fun simulateRaceConditionWithScheduledTask(makeProjectDisposed: () -> Unit): AlreadyDisposedException? {
            var caughtException: AlreadyDisposedException? = null
            val future = AppExecutorUtil.getAppScheduledExecutorService().submit {
                try {
                    latch.await(1, TimeUnit.SECONDS)
                    capabilities = LSPCapabilities(cloudName = "test-cloud")
                } catch (e: Exception) {
                    if (e is AlreadyDisposedException) {
                        caughtException = e
                    }
                }
            }

            makeProjectDisposed()
            latch.countDown()
            future.get(2, TimeUnit.SECONDS)
            return caughtException
        }

        override fun ensureStartedBlocking(reason: String) {
            if (blockEnsureStartedBlocking) {
                ensureStartedBlockingEntered.countDown()
                releaseEnsureStartedBlocking.await(2, TimeUnit.SECONDS)
            }
            super.ensureStartedBlocking(reason)
        }

        fun ensureStartedBlockingForTest(reason: String) {
            ensureStartedBlocking(reason)
        }

        fun runHealthCheckOnceForTest() {
            runHealthCheckOnce()
        }

        fun advanceHealthClock(ms: Long) {
            healthClockMs.addAndGet(ms)
        }

        fun setBinaryResolutionResults(vararg results: String?) {
            binaryResolutionResults.clear()
            binaryResolutionResults.addAll(results.toList())
            binaryResolutionResultIndex.set(0)
            binaryResolutionCalls.set(0)
        }

        fun hasBinaryResolutionFailureForTest(): Boolean {
            val field = LSPProcessHolder::class.java.getDeclaredField("binaryResolutionFailure")
            field.isAccessible = true
            return field.get(this) != null
        }
    }

    @Test
    fun testAttemptingToReachWithoutEngineDoesNotReportPortZero() {
        val holder = TestLspProcessHolder(mockProject(createTempDir().canonicalPath), FakeEngineClient())
        try {
            assertEquals("Refact engine (starting)", holder.attempingToReach())
        } finally {
            holder.dispose()
        }
    }

    private fun mockProject(root: String? = null): Project {
        return mockProject(root, mock(LSPProcessHolderChangedNotifier::class.java))
    }

    private fun mockProject(root: String? = null, publisher: LSPProcessHolderChangedNotifier): Project {
        val mockProject = mock(Project::class.java)
        val mockMessageBus = mock(MessageBus::class.java)
        `when`(mockProject.isDisposed).thenReturn(false)
        `when`(mockProject.messageBus).thenReturn(mockMessageBus)
        `when`(mockMessageBus.syncPublisher(LSPProcessHolderChangedNotifier.TOPIC)).thenReturn(publisher)
        if (root != null) {
            `when`(mockProject.basePath).thenReturn(root)
        }
        return mockProject
    }

    private class RecordingStatusPublisher : LSPProcessHolderChangedNotifier {
        val statuses = Collections.synchronizedList(mutableListOf<LSPBackendConnectionStatus>())

        override fun backendConnectionStatusChanged(newStatus: LSPBackendConnectionStatus) {
            statuses.add(newStatus)
        }
    }

    private fun <T> runOffEdt(block: () -> T): T {
        return ApplicationManager.getApplication().executeOnPooledThread<T> { block() }.get(10, TimeUnit.SECONDS)
    }

    private fun withServer(block: (MockWebServer) -> Unit) {
        val server = MockWebServer()
        try {
            server.protocols = listOf(Protocol.H2_PRIOR_KNOWLEDGE)
            server.start()
            block(server)
        } finally {
            server.shutdown()
        }
    }

    private fun waitForLifecycle(holder: TestLspProcessHolder) {
        val deadline = System.currentTimeMillis() + 3000
        while (holder.hasPendingLifecycleWork() && System.currentTimeMillis() < deadline) {
            Thread.sleep(25)
        }
        if (holder.hasPendingLifecycleWork()) {
            Thread.sleep(25)
        }
        assertFalse(holder.hasPendingLifecycleWork())
    }

    private fun withBrowserHost(host: String, block: () -> Unit) {
        val previous = InferenceGlobalContext.browserHost
        try {
            InferenceGlobalContext.browserHost = host
            block()
        } finally {
            InferenceGlobalContext.browserHost = previous
        }
    }

    @Test
    fun testAlreadyDisposedException() {
        val mockProject = mockProject()
        val holder = TestLspProcessHolder(mockProject, FakeEngineClient())

        val exception = holder.simulateRaceConditionWithScheduledTask {
            `when`(mockProject.isDisposed).thenReturn(true)
            `when`(mockProject.messageBus).thenThrow(
                AlreadyDisposedException("Already disposed")
            )
        }

        assertNull("With the fix, no AlreadyDisposedException should be thrown", exception)
        assertEquals("test-cloud", holder.capabilities.cloudName)
        holder.dispose()
    }

    @Test
    fun testConcurrentBlockingEnsureStartedOnlySpawnsOnce() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        val executor = Executors.newFixedThreadPool(3)
        try {
            val futures = (1..3).map {
                executor.submit {
                    holder.ensureStartedBlockingForTest("concurrent-$it")
                }
            }

            futures.forEach { it.get(3, TimeUnit.SECONDS) }

            assertEquals(1, fake.spawnCalls.get())
        } finally {
            executor.shutdownNow()
            holder.dispose()
        }
    }

    @Test
    fun testSpawnsEngineAndUsesEngineRootUrl() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("engine-spawn") }

            assertEquals(1, fake.spawnCalls.get())
            assertEquals(root, fake.spawnedRoots.single())
            assertEquals(URI("http://127.0.0.1:18081/"), holder.baseUrlOrNull())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testSpawnFailureRecordsBinaryResolutionState() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient().apply { spawnError = RuntimeException("spawn failed") }
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("engine-spawn-failure") }

            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())
            assertFalse(holder.backendReady())
            assertFalse(holder.isWorking)
            assertNull(holder.baseUrlOrNull())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testBrowserUrlUsesExplicitBrowserHost() = withBrowserHost("machine.local") {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("browser-url-override") }

            assertEquals(URI("http://machine.local:18081/"), holder.browserUrlOrNull())
            assertEquals(URI("http://127.0.0.1:18081/"), holder.embeddedBrowserUrlOrNull())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testRefreshedWorkerStateHitsEngineRootEndpoints() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient().apply {
            spawnHandle = EngineHandle(httpPort = 0, lspPort = 0, owned = true, pid = null)
        }
        val holder = TestLspProcessHolder(mockProject(root), fake, refreshWorkerState = true)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        val previousConnection = InferenceGlobalContext.connection
        val testConnection = AsyncConnection()
        InferenceGlobalContext.connection = testConnection
        try {
            withServer { server ->
                fake.spawnHandle = EngineHandle(httpPort = server.port, lspPort = server.port, owned = true, pid = null)
                server.enqueue(
                    MockResponse.Builder()
                        .code(200)
                        .body("worker build")
                        .build()
                )
                server.enqueue(
                    MockResponse.Builder()
                        .code(200)
                        .addHeader("Content-Type", "application/json")
                        .body("{}")
                        .build()
                )
                server.enqueue(
                    MockResponse.Builder()
                        .code(200)
                        .addHeader("Content-Type", "application/json")
                        .body("{}")
                        .build()
                )

                runOffEdt { holder.ensureStartedBlockingForTest("build-info") }

                assertEquals("worker build", LSPProcessHolder.buildInfo)
                assertEquals("/v1/build_info", server.takeRequest().path)
                assertEquals("/v1/caps", server.takeRequest().path)
                assertEquals("/v1/customization", server.takeRequest().path)
            }
        } finally {
            InferenceGlobalContext.connection = previousConnection
            testConnection.dispose()
            holder.dispose()
        }
    }

    @Test
    fun testDisposeKillsOwnedEngineAndForgetsAttachState() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"

        runOffEdt { holder.ensureStartedBlockingForTest("dispose") }
        assertNotNull(holder.baseUrlOrNull())
        holder.dispose()

        assertNull(holder.baseUrlOrNull())
        assertEquals(1, fake.spawnCalls.get())
        assertEquals(1, fake.killCalls.get())
        assertEquals("owned", fake.killOrders.single())
    }

    @Test
    fun testFetchCustomizationRetriesAfterTransientWakeFailure() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        val previousConnection = InferenceGlobalContext.connection
        val testConnection = AsyncConnection()
        InferenceGlobalContext.connection = testConnection
        try {
            withServer { server ->
                fake.spawnHandle = EngineHandle(httpPort = server.port, lspPort = server.port, owned = true, pid = null)
                server.enqueue(
                    MockResponse.Builder()
                        .code(503)
                        .addHeader("Content-Type", "application/json")
                        .body("worker warming")
                        .build()
                )
                server.enqueue(
                    MockResponse.Builder()
                        .code(200)
                        .addHeader("Content-Type", "application/json")
                        .body("{\"code_lens\":{}}")
                        .build()
                )

                runOffEdt { holder.ensureStartedBlockingForTest("customization") }
                val customization = runOffEdt { holder.fetchCustomization() }

                assertNotNull(customization)
                assertTrue(customization!!.has("code_lens"))
                assertEquals(2, fake.spawnCalls.get())
                assertEquals(listOf(1), holder.retryAttempts)
                assertEquals("/v1/customization", server.takeRequest().path)
                assertEquals("/v1/customization", server.takeRequest().path)
            }
        } finally {
            InferenceGlobalContext.connection = previousConnection
            testConnection.dispose()
            holder.dispose()
        }
    }

    @Test
    fun testAsyncConnectionNon2xxFailsWithBody() {
        val previousConnection = InferenceGlobalContext.connection
        val connection = AsyncConnection()
        InferenceGlobalContext.connection = connection
        try {
            withServer { server ->
                server.enqueue(
                    MockResponse.Builder()
                        .code(503)
                        .addHeader("Content-Type", "application/json")
                        .body("wake me")
                        .build()
                )

                val future = connection.get(URI(server.url("/v1/caps").toString())).get()

                try {
                    future.get()
                    fail("non-2xx response should fail")
                } catch (error: Exception) {
                    val cause = error.cause
                    assertTrue("expected HttpStatusException, got $cause", cause is HttpStatusException)
                    val status = cause as HttpStatusException
                    assertEquals(503, status.statusCode)
                    assertEquals("wake me", status.responseBody)
                }
            }
        } finally {
            InferenceGlobalContext.connection = previousConnection
            connection.dispose()
        }
    }

    @Test
    fun testPendingLifecycleFlagDefaultsFalse() {
        val holder = TestLspProcessHolder(mockProject(), FakeEngineClient())
        assertFalse(holder.hasPendingLifecycleWork())
        holder.dispose()
    }

    @Test
    fun testBackendConnectionStatusDefaultsConnecting() {
        val holder = TestLspProcessHolder(mockProject(), FakeEngineClient())
        assertEquals(LSPBackendConnectionStatus.CONNECTING, holder.backendConnectionStatus())
        assertFalse(holder.backendReady())
        holder.dispose()
    }

    @Test
    fun testBackendConnectionStatusReadyAfterAttach() {
        val root = createTempDir().canonicalPath
        val holder = TestLspProcessHolder(mockProject(root), FakeEngineClient())
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("backend-ready") }

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertTrue(holder.backendReady())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testBinaryResolutionPublishesInstallingThenFailureAndSettingsResetRecovers() {
        val root = createTempDir().canonicalPath
        val publisher = RecordingStatusPublisher()
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root, publisher), fake).apply {
            blockBinaryResolution = true
            signalDownloadStartDuringResolution = true
            setBinaryResolutionResults(null, "/tmp/refact-after-reset")
        }
        LSPProcessHolder.BIN_PATH = null
        val future = ApplicationManager.getApplication().executeOnPooledThread {
            holder.ensureStartedBlockingForTest("binary-slow-failure")
        }
        try {
            assertTrue(holder.binaryResolutionEntered.await(2, TimeUnit.SECONDS))
            assertEquals(LSPBackendConnectionStatus.INSTALLING, holder.backendConnectionStatus())
            holder.releaseBinaryResolution.countDown()
            future.get(3, TimeUnit.SECONDS)

            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())
            assertTrue(holder.hasBinaryResolutionFailureForTest())
            assertEquals(1, holder.binaryResolutionCalls.get())
            assertTrue(publisher.statuses.contains(LSPBackendConnectionStatus.INSTALLING))
            assertEquals(LSPBackendConnectionStatus.FAILED, publisher.statuses.last())

            fake.spawnError = null
            holder.advanceHealthClock(60_000)
            holder.settingsChanged("binary-path-changed")
            waitForLifecycle(holder)

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertFalse(holder.hasBinaryResolutionFailureForTest())
            assertEquals(2, holder.binaryResolutionCalls.get())
            assertEquals(listOf("/tmp/refact-after-reset"), fake.spawnedBinPaths)
        } finally {
            holder.releaseBinaryResolution.countDown()
            holder.dispose()
        }
    }

    @Test
    fun testBackendReadyWaitsForInitialize() = withBrowserHost("") {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake).apply {
            blockInitialize = true
        }
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        val future = ApplicationManager.getApplication().executeOnPooledThread {
            holder.ensureStartedBlockingForTest("backend-ready-after-init")
        }
        val expectedUrl = URI("http://127.0.0.1:18081/")
        try {
            assertTrue(holder.initializeEntered.await(2, TimeUnit.SECONDS))
            assertEquals(LSPBackendConnectionStatus.STARTING, holder.backendConnectionStatus())
            assertFalse(holder.backendReady())
            assertTrue(holder.isWorking)
            assertNotNull(holder.baseUrlOrNull())
            assertEquals(expectedUrl, holder.browserUrlOrNull())
            assertNull(holder.embeddedBrowserUrlOrNull())

            holder.releaseInitialize.countDown()
            future.get(3, TimeUnit.SECONDS)

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertTrue(holder.backendReady())
            assertEquals(expectedUrl, holder.browserUrlOrNull())
            assertEquals(expectedUrl, holder.embeddedBrowserUrlOrNull())
        } finally {
            holder.releaseInitialize.countDown()
            holder.dispose()
        }
    }

    @Test
    fun testInitializeFailureDoesNotPublishReady() {
        val root = createTempDir().canonicalPath
        val holder = TestLspProcessHolder(mockProject(root), FakeEngineClient()).apply {
            initializeError = RuntimeException("init failed")
        }
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("init-failure") }

            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())
            assertFalse(holder.backendReady())
            assertFalse(holder.isWorking)
            assertNull(holder.baseUrlOrNull())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testBackendConnectionStatusFailedAfterAttachError() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient().apply {
            spawnError = RuntimeException("spawn failed")
        }
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("backend-failed") }

            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())
            assertFalse(holder.backendReady())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testFailedStateHealthCheckRetriesAndRecovers() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient().apply {
            spawnError = RuntimeException("first spawn failed")
        }
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("transient-failure") }
            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())
            assertNull(holder.baseUrlOrNull())

            fake.spawnError = null
            holder.advanceHealthClock(1_000)
            holder.runHealthCheckOnceForTest()
            waitForLifecycle(holder)
            holder.advanceHealthClock(60_000)

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertTrue(holder.backendReady())
            assertEquals(2, fake.spawnCalls.get())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testBinaryResolutionFailureBacksOffAndSettingsResetAllowsRetry() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake).apply {
            setBinaryResolutionResults(null, "/tmp/refact-after-reset")
        }
        LSPProcessHolder.BIN_PATH = null
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("binary-missing") }

            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())
            assertTrue(holder.hasBinaryResolutionFailureForTest())
            assertEquals(1, holder.binaryResolutionCalls.get())
            assertEquals(0, fake.spawnCalls.get())

            holder.runHealthCheckOnceForTest()
            waitForLifecycle(holder)
            runOffEdt { holder.ensureStartedBlockingForTest("suppressed-manual-retry") }

            assertEquals(1, holder.binaryResolutionCalls.get())
            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())

            fake.spawnError = null
            holder.advanceHealthClock(60_000)
            holder.settingsChanged("binary-path-changed")
            waitForLifecycle(holder)

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertFalse(holder.hasBinaryResolutionFailureForTest())
            assertEquals(2, holder.binaryResolutionCalls.get())
            assertEquals(listOf("/tmp/refact-after-reset"), fake.spawnedBinPaths)
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testBinaryResolutionFailureRetriesAfterBackoffAndSuccessClearsCache() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake).apply {
            setBinaryResolutionResults(null, "/tmp/refact-after-backoff")
        }
        LSPProcessHolder.BIN_PATH = null
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("binary-missing") }

            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())
            assertTrue(holder.hasBinaryResolutionFailureForTest())
            assertEquals(1, holder.binaryResolutionCalls.get())

            holder.advanceHealthClock(999)
            holder.runHealthCheckOnceForTest()
            waitForLifecycle(holder)
            assertEquals(1, holder.binaryResolutionCalls.get())

            holder.advanceHealthClock(1)
            holder.runHealthCheckOnceForTest()
            waitForLifecycle(holder)

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertFalse(holder.hasBinaryResolutionFailureForTest())
            assertEquals(2, holder.binaryResolutionCalls.get())
            assertEquals(listOf("/tmp/refact-after-backoff"), fake.spawnedBinPaths)
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testSingleFailedHealthProbeKeepsBackendReady() {
        val root = createTempDir().canonicalPath
        val publisher = RecordingStatusPublisher()
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root, publisher), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("health-probe-start") }
            publisher.statuses.clear()

            holder.probeResult = false
            holder.runHealthCheckOnceForTest()

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertTrue(holder.backendReady())
            assertTrue(holder.isWorking)
            assertNotNull(holder.baseUrlOrNull())
            assertFalse(holder.hasPendingLifecycleWork())
            assertEquals(0, fake.killCalls.get())
            assertFalse(publisher.statuses.contains(LSPBackendConnectionStatus.FAILED))

            holder.probeResult = true
            holder.runHealthCheckOnceForTest()

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertEquals(1, fake.spawnCalls.get())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testSuccessfulHealthProbeResetsFailedProbeCounter() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("health-probe-start") }

            holder.probeResult = false
            holder.runHealthCheckOnceForTest()
            holder.runHealthCheckOnceForTest()
            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())

            holder.probeResult = true
            holder.runHealthCheckOnceForTest()

            holder.probeResult = false
            holder.runHealthCheckOnceForTest()
            holder.runHealthCheckOnceForTest()

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertTrue(holder.isWorking)
            assertNotNull(holder.baseUrlOrNull())
            assertFalse(holder.hasPendingLifecycleWork())
            assertEquals(1, fake.spawnCalls.get())
            assertEquals(0, fake.killCalls.get())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testSettingsChangeResetsFailedProbeCounter() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("health-probe-start") }

            holder.probeResult = false
            holder.runHealthCheckOnceForTest()
            holder.runHealthCheckOnceForTest()
            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())

            holder.settingsChanged("health-counter-reset")
            waitForLifecycle(holder)
            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())

            holder.runHealthCheckOnceForTest()
            holder.runHealthCheckOnceForTest()

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertTrue(holder.isWorking)
            assertNotNull(holder.baseUrlOrNull())
            assertFalse(holder.hasPendingLifecycleWork())
            assertEquals(1, fake.spawnCalls.get())
            assertEquals(0, fake.killCalls.get())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testFailedHealthProbeClearsStateAndBacksOff() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("health-probe-start") }
            assertNotNull(holder.baseUrlOrNull())

            holder.blockEnsureStartedBlocking = true
            holder.probeResult = false
            holder.runHealthCheckOnceForTest()
            holder.runHealthCheckOnceForTest()
            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertTrue(holder.isWorking)
            assertNotNull(holder.baseUrlOrNull())
            assertFalse(holder.hasPendingLifecycleWork())

            holder.runHealthCheckOnceForTest()
            assertTrue(holder.ensureStartedBlockingEntered.await(2, TimeUnit.SECONDS))

            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())
            assertFalse(holder.isWorking)
            assertNull(holder.baseUrlOrNull())

            holder.probeResult = true
            holder.runHealthCheckOnceForTest()
            assertEquals(1, fake.spawnCalls.get())

            holder.advanceHealthClock(1_000)
            holder.releaseEnsureStartedBlocking.countDown()
            waitForLifecycle(holder)
            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertEquals(2, fake.spawnCalls.get())
        } finally {
            holder.releaseEnsureStartedBlocking.countDown()
            holder.dispose()
        }
    }

    @Test
    fun testWakeRetryRunsInitialize() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("wake-init-start") }
            assertEquals(1, holder.initializeCalls.get())

            val recovered = runOffEdt { holder.wakeWorkerForRetry("wake-init") }

            assertTrue(recovered)
            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertEquals(2, holder.initializeCalls.get())
            assertEquals(2, fake.spawnCalls.get())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testWakeRetryRegistersSharedBinaryForTerminal() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("wake-terminal-registration-start") }
            holder.terminalRegistrationPaths.clear()

            val recovered = runOffEdt { holder.wakeWorkerForRetry("wake-terminal-registration") }

            assertTrue(recovered)
            assertEquals(listOf("/tmp/refact"), holder.terminalRegistrationPaths)
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testWakeRetryDoesNotPublishInstallingWhenCachedBinaryResolves() {
        val root = createTempDir().canonicalPath
        val publisher = RecordingStatusPublisher()
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root, publisher), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact-wake"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("wake-installing-start") }
            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())

            publisher.statuses.clear()
            val recovered = runOffEdt { holder.wakeWorkerForRetry("wake-installing") }

            assertTrue(recovered)
            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertFalse(publisher.statuses.contains(LSPBackendConnectionStatus.INSTALLING))
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testFailedWakeClearsAttachedWorkerState() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient()
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("wake-failure-start") }
            assertNotNull(holder.baseUrlOrNull())

            fake.spawnError = RuntimeException("wake failed")
            val recovered = runOffEdt { holder.wakeWorkerForRetry("wake-failure") }

            assertFalse(recovered)
            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())
            assertFalse(holder.isWorking)
            assertNull(holder.baseUrlOrNull())
        } finally {
            holder.dispose()
        }
    }

    @Test
    fun testHttp504IsRecoverable() {
        assertTrue(isRecoverableHttpStatus(HttpStatusException(504, "timeout", URI("http://127.0.0.1"))))
        assertTrue(isRecoverableHttpStatus(HttpStatusException(500, "internal error", URI("http://127.0.0.1"))))
        assertFalse(isRecoverableHttpStatus(HttpStatusException(499, "client closed", URI("http://127.0.0.1"))))
    }

    @Test
    fun testEnsureStartedAsyncRetriesAfterFailedStatus() {
        val root = createTempDir().canonicalPath
        val fake = FakeEngineClient().apply {
            spawnError = RuntimeException("first spawn failed")
        }
        val holder = TestLspProcessHolder(mockProject(root), fake)
        LSPProcessHolder.BIN_PATH = "/tmp/refact"
        try {
            runOffEdt { holder.ensureStartedBlockingForTest("backend-failed") }
            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())

            fake.spawnError = null
            holder.blockEnsureStartedBlocking = true
            holder.ensureStartedAsync("failed-config-request")

            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())
            assertTrue(holder.ensureStartedBlockingEntered.await(2, TimeUnit.SECONDS))
            assertEquals(LSPBackendConnectionStatus.FAILED, holder.backendConnectionStatus())

            holder.releaseEnsureStartedBlocking.countDown()
            waitForLifecycle(holder)

            assertEquals(LSPBackendConnectionStatus.READY, holder.backendConnectionStatus())
            assertTrue(holder.backendReady())
            assertEquals(2, fake.spawnCalls.get())
        } finally {
            holder.releaseEnsureStartedBlocking.countDown()
            holder.dispose()
        }
    }

    @Test
    fun testConfigPayloadContainsBackendReadiness() {
        val payload = Events.Config.UpdatePayload(
            Events.Config.Features(true, false),
            Events.Config.ThemeProps("light"),
            0,
            Events.Config.KeyBindings("foo"),
            backendReady = false,
            connectionStatus = LSPBackendConnectionStatus.STARTING.wireName
        )

        val result = Events.stringify(Events.Config.Update(payload))

        assertTrue(result.contains("\"backendReady\":false"))
        assertTrue(result.contains("\"connectionStatus\":\"starting\""))
    }
}







