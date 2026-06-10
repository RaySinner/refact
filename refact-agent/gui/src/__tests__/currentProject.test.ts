import { describe, expect, it } from "vitest";
import {
  currentProjectInfoReducer,
  resetSidebarReadiness,
  selectHasActiveProject,
  setCurrentProjectInfo,
} from "../features/Chat/currentProject";
import { setUpStore } from "../app/store";

describe("currentProjectInfoReducer", () => {
  it("preserves workspace roots when the same project update omits roots", () => {
    let state = currentProjectInfoReducer(
      undefined,
      setCurrentProjectInfo({
        name: "refact",
        workspaceRoots: ["/tmp/a/refact"],
      }),
    );

    state = currentProjectInfoReducer(
      state,
      setCurrentProjectInfo({ name: "refact" }),
    );

    expect(state.workspaceRoots).toEqual(["/tmp/a/refact"]);
  });

  it("uses workspace roots, not matching names, for known project identity", () => {
    let state = currentProjectInfoReducer(
      undefined,
      setCurrentProjectInfo({
        name: "refact",
        workspaceRoots: ["/tmp/a/refact"],
      }),
    );

    state = currentProjectInfoReducer(
      state,
      setCurrentProjectInfo({
        name: "refact",
        workspaceRoots: ["/tmp/b/refact"],
      }),
    );

    expect(state.workspaceRoots).toEqual(["/tmp/b/refact"]);
  });

  it("does not reset project identity for the compatibility readiness reset action", () => {
    const state = currentProjectInfoReducer(
      {
        name: "refact",
        workspaceRoots: ["/tmp/a/refact"],
      },
      resetSidebarReadiness(),
    );

    expect(state).toEqual({
      name: "refact",
      workspaceRoots: ["/tmp/a/refact"],
    });
  });

  it("treats a name-only project as active", () => {
    const store = setUpStore({
      current_project: { name: "refact" },
    });

    expect(selectHasActiveProject(store.getState())).toBe(true);
  });

  it("keeps an empty-roots project active when it still has a name", () => {
    const store = setUpStore({
      current_project: { name: "refact", workspaceRoots: [] },
    });

    expect(selectHasActiveProject(store.getState())).toBe(true);
  });

  it("does not treat an unnamed empty-roots backend snapshot as active", () => {
    const store = setUpStore({
      current_project: { name: "", workspaceRoots: [] },
    });

    expect(selectHasActiveProject(store.getState())).toBe(false);
  });

  it("does not let config fallback make an unnamed empty-roots snapshot active", () => {
    const store = setUpStore({
      config: {
        host: "vscode",
        lspPort: 8001,
        themeProps: {},
        currentWorkspaceName: "refact",
      },
      current_project: { name: "", workspaceRoots: [] },
    });

    expect(selectHasActiveProject(store.getState())).toBe(false);
  });
});
