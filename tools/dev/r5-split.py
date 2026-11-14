"""R-5 generator: split `refact-agent/engine/src/app_state.rs` into `app_state/*.rs`.

The card asks for (a) a submodule split of the last god-file in `engine/src`, keeping
the `pub use` surface byte-identical, and (b) a per-item classification of what can be
harvested into a crate. The census (`tools/dev/chat_movable_census.py app_state`) says
MOVABLE now = 0: everything left either takes `Arc<GlobalContext>`/`AppState` (the
deliberate `ContextFactory` seam) or is already a `pub use` re-export. So the card is
satisfied by the split plus an explicit island list.

Run BEFORE deleting the original file - the script deletes it itself:

    python tools/dev/r5-split.py --write

Layout (base line ranges, 1-based, of `60c7da99f:refact-agent/engine/src/app_state.rs`)
-------------------------------------------------------------------------------------
app_state.rs                  imports/`pub use` surface + `mod`/glob declarations +
                              the intended islands (`EngineContextFactory`,
                              `EngineCapsLoader`, `EngineExecPolicyProvider`,
                              `EngineFilesProvider`) + the verbatim test module
app_state/facade.rs           56-63, 66-273    EngineBuddyServicesHandle,
                                               EngineChatSessionFacade
app_state/launchers.rs        519-724, 726-739 EngineSubchatLauncher,
                                               EngineBackgroundAgentSpawner
app_state/providers.rs        741-858          ProjectTree/Hook/ModeConfig/Caps providers
app_state/trajectory_store.rs 1010-1289        EngineTrajectoryStore
app_state/prompt_and_tools.rs 1290-1381        EnginePromptAssembler,
                                               EngineChatToolExecutor
app_state/sinks.rs            1382-1400,       AppActivitySink, AppBuddyEventSink,
                              2428-2642        Engine{Trajectory,Task,Notification}
                                               EventSender
app_state/tool_pool.rs        1407, 1409-1478  MutableTool, AppTurnToolPool,
                                               ToolPoolExpansionGuard
app_state/tool_registry.rs    1401-1405,       AppToolRegistry + FixtureToolFactory
                              1482-1809        + the registry inherent impl
app_state/tool_registry_trait.rs 1815-2427     first_tool_text + `impl ToolRegistry`

Rules
-----
* every block is copied verbatim except for visibility widening, applied only on the
  lines listed in WIDENING and asserted by count;
* a child carries the base's private imports through `use super::*;` (rustc re-exports
  the parent's private `use`s to descendants, so no import needs duplicating);
* `pub(super)` is the exact old single-module scope: visible in `app_state` and every
  descendant (siblings and the parent's `mod tests`), invisible to the rest of the
  crate. Verified against rustc before writing this script.
"""
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENGINE = ROOT / "refact-agent" / "engine"
SOURCE_FILE = ENGINE / "src" / "app_state.rs"
CHILD_DIR = ENGINE / "src" / "app_state"
BASE_COMMIT = "60c7da99f"
BASE_REL = "refact-agent/engine/src/app_state.rs"

# (destination, first_line, last_line) - 1-based, inclusive
BLOCKS = [
    ("parent", 1, 55),
    ("facade", 56, 63),
    ("parent", 64, 65),
    ("facade", 66, 273),
    ("parent", 274, 518),
    ("launchers", 519, 740),
    ("providers", 741, 858),
    ("parent", 859, 1009),
    ("trajectory_store", 1010, 1289),
    ("prompt_and_tools", 1290, 1381),
    ("sinks", 1382, 1400),
    ("tool_registry", 1401, 1406),
    ("tool_pool", 1407, 1479),
    ("parent", 1480, 1481),
    ("tool_registry", 1482, 1810),
    ("parent", 1811, 1814),
    ("tool_registry_trait", 1815, 2427),
    ("sinks", 2428, 2642),
    ("parent", 2643, 3235),
]

# base line -> (exact old line, exact new line); asserted line by line
WIDENING = [
    (1402, "    gcx: SharedGlobalContext,",
           "    pub(super) gcx: SharedGlobalContext,"),
    (1407, "type MutableTool = Box<dyn crate::tools::tools_description::Tool + Send>;",
           "pub(super) type MutableTool = Box<dyn crate::tools::tools_description::Tool + Send>;"),
    (1409, "struct AppTurnToolPool {",
           "pub(super) struct AppTurnToolPool {"),
    (1410, "    tools: AMutex<HashMap<String, VecDeque<MutableTool>>>,",
           "    pub(super) tools: AMutex<HashMap<String, VecDeque<MutableTool>>>,"),
    (1411, "    expansion_in_progress: AtomicBool,",
           "    pub(super) expansion_in_progress: AtomicBool,"),
    (1412, "    expansion_finished: Notify,",
           "    pub(super) expansion_finished: Notify,"),
    (1415, "struct ToolPoolExpansionGuard<'a> {",
           "pub(super) struct ToolPoolExpansionGuard<'a> {"),
    (1416, "    pool: &'a AppTurnToolPool,",
           "    pub(super) pool: &'a AppTurnToolPool,"),
    (1429, "    fn tool_key(desc: &refact_tool_api::ToolDesc) -> String {",
           "    pub(super) fn tool_key(desc: &refact_tool_api::ToolDesc) -> String {"),
    (1438, "    fn from_tools(tools: Vec<MutableTool>) -> Self {",
           "    pub(super) fn from_tools(tools: Vec<MutableTool>) -> Self {"),
    (1453, "    async fn take(&self, desc: &refact_tool_api::ToolDesc) -> Option<MutableTool> {",
           "    pub(super) async fn take(&self, desc: &refact_tool_api::ToolDesc) -> Option<MutableTool> {"),
    (1461, "    async fn return_tool(&self, tool: MutableTool) {",
           "    pub(super) async fn return_tool(&self, tool: MutableTool) {"),
    (1471, "    async fn add_missing_tools(&self, tools: Vec<MutableTool>) {",
           "    pub(super) async fn add_missing_tools(&self, tools: Vec<MutableTool>) {"),
    (1506, "    async fn tools_for_mode(",
           "    pub(super) async fn tools_for_mode("),
    (1522, "    fn snapshot_cache_enabled() -> bool {",
           "    pub(super) fn snapshot_cache_enabled() -> bool {"),
    (1527, "    fn snapshot_cache_enabled_for(value: Option<&str>) -> bool {",
           "    pub(super) fn snapshot_cache_enabled_for(value: Option<&str>) -> bool {"),
    (1589, "    async fn snapshot_for_mode(",
           "    pub(super) async fn snapshot_for_mode("),
    (1599, "    async fn execution_scope_from_ccx(",
           "    pub(super) async fn execution_scope_from_ccx("),
    (1609, "    async fn fresh_mutable_tools(",
           "    pub(super) async fn fresh_mutable_tools("),
    (1631, "    async fn build_turn_tool_pool(",
           "    pub(super) async fn build_turn_tool_pool("),
    (1652, "    fn app_turn_tool_pool<'a>(pool: &'a TurnToolPool) -> Result<&'a AppTurnToolPool, String> {",
           "    pub(super) fn app_turn_tool_pool<'a>(pool: &'a TurnToolPool) -> Result<&'a AppTurnToolPool, String> {"),
    (1657, "    async fn add_turn_tool_pool_fallback(",
           "    pub(super) async fn add_turn_tool_pool_fallback("),
    (1681, "    async fn take_turn_tool(",
           "    pub(super) async fn take_turn_tool("),
    (1731, "    fn find_catalog_descriptor<'a>(",
           "    pub(super) fn find_catalog_descriptor<'a>("),
    (1743, "    async fn snapshot_for_mode_with_scope(",
           "    pub(super) async fn snapshot_for_mode_with_scope("),
    (1789, "    async fn snapshot_for_mode_for_test(",
           "    pub(super) async fn snapshot_for_mode_for_test("),
    (1800, "    async fn snapshot_for_mode_with_scope_for_test(",
           "    pub(super) async fn snapshot_for_mode_with_scope_for_test("),
]

CHILDREN = [
    "facade",
    "launchers",
    "prompt_and_tools",
    "providers",
    "sinks",
    "tool_pool",
    "tool_registry",
    "tool_registry_trait",
    "trajectory_store",
]

CHILD_HEADER = "#[allow(unused_imports)]\nuse super::*;\n\n"


def base_lines():
    raw = subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:%s" % (BASE_COMMIT, BASE_REL)],
        capture_output=True,
        check=True,
    ).stdout.decode("utf-8")
    raw = raw.replace("\r\n", "\n")
    assert raw.endswith("\n"), "base file must end with a newline"
    return raw[:-1].split("\n")


def main():
    write = "--write" in sys.argv
    lines = base_lines()

    covered = set()
    owners = {}
    for destination, first, last in BLOCKS:
        assert 1 <= first <= last <= len(lines), (destination, first, last)
        for number in range(first, last + 1):
            assert number not in covered, "block overlap at line %d" % number
            covered.add(number)
            owners[number] = destination
    for number in range(1, len(lines) + 1):
        if number not in covered:
            assert lines[number - 1].strip() == "", \
                "uncovered non-blank line %d: %r" % (number, lines[number - 1])

    for number, old, _new in WIDENING:
        assert lines[number - 1] == old, \
            "widening target %d is %r, expected %r" % (number, lines[number - 1], old)

    pieces = {name: [] for name in CHILDREN}
    parent_pieces = []
    for destination, first, last in BLOCKS:
        body = lines[first - 1:last]
        body = [text for text in body]
        for number, old, new in WIDENING:
            if first <= number <= last:
                index = number - first
                assert body[index] == old
                body[index] = new
        if destination == "parent":
            parent_pieces.append((first, body))
        else:
            pieces[destination].append((first, body))

    parent_text = []
    for first, body in parent_pieces:
        if first == 274:
            for name in CHILDREN:
                parent_text.append("mod %s;" % name)
            parent_text.append("")
            for name in CHILDREN:
                parent_text.append("#[allow(unused_imports)]")
                parent_text.append("pub use self::%s::*;" % name)
            parent_text.append("")
        parent_text.extend(body)
    parent_out = "".join(line + "\n" for line in parent_text)

    child_out = {}
    for name in CHILDREN:
        chunks = []
        for _first, body in pieces[name]:
            chunks.extend(body)
        child_out[name] = CHILD_HEADER + "".join(line + "\n" for line in chunks)

    total_new = len(parent_out.split("\n")) + sum(
        len(child_out[name].split("\n")) for name in CHILDREN
    )
    print("parent: %d lines" % len(parent_out.split("\n")))
    for name in CHILDREN:
        print("  app_state/%-22s %4d lines (%d blocks)" % (
            name + ".rs", len(child_out[name].split("\n")), len(pieces[name])))

    if not write:
        print("\n(dry run; pass --write to apply)")
        return 0

    CHILD_DIR.mkdir(parents=True, exist_ok=True)
    for name in CHILDREN:
        (CHILD_DIR / (name + ".rs")).write_text(child_out[name], encoding="utf-8", newline="\n")
    SOURCE_FILE.write_text(parent_out, encoding="utf-8", newline="\n")
    print("\nwrote %d files; %d lines total" % (len(CHILDREN) + 1, total_new))
    return 0


if __name__ == "__main__":
    sys.exit(main())
