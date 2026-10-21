# JetBrains: verification status

The plugin **compiles** (`compileKotlin` / `compileTestKotlin`) and its **181 unit tests pass** on the dev machine. It has **never been exercised in a real JetBrains IDE** — no JetBrains IDE is available on the dev machine, so "tests green" does not mean "works in a real IDE".

Before considering engine-per-window integration verified in JetBrains, run the following manual smoke checklist:

1. Open a project → the engine spawns, no console window flashes, and the status bar shows a real port (not 0).
2. Close the IDE window → the engine process is gone (check Task Manager).
3. Reload the IDE → a new engine starts, with no orphan from the previous run.
4. Open two IDE windows on the same folder → the second one adopts the first's engine or shows an actionable message.
5. Change an indexing setting → the engine restarts on new ephemeral ports and chat still works.
6. Set `xDebugLSPPort` → the plugin attaches to the running engine instead of spawning.
7. Kill the engine externally → the health check recovers within its backoff.
