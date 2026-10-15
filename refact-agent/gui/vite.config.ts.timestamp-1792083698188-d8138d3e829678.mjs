// vite.config.ts
import path from "path";
import { defineConfig } from "file:///C:/Raid/Repos/Rust/refact-main/refact-agent/gui/node_modules/vite/dist/node/index.js";
import react from "file:///C:/Raid/Repos/Rust/refact-main/refact-agent/gui/node_modules/@vitejs/plugin-react-swc/index.mjs";
import eslint from "file:///C:/Raid/Repos/Rust/refact-main/refact-agent/gui/node_modules/vite-plugin-eslint/dist/index.mjs";
import { configDefaults, coverageConfigDefaults } from "file:///C:/Raid/Repos/Rust/refact-main/refact-agent/gui/node_modules/vitest/dist/config.js";
import dts from "file:///C:/Raid/Repos/Rust/refact-main/refact-agent/gui/node_modules/vite-plugin-dts/dist/index.mjs";
import { execSync } from "child_process";
var __vite_injected_original_dirname = "C:\\Users\\raymond\\.cache\\refact\\worktrees\\8a0bca400273025f\\3e9bc25c-678b-4f05-bcc5-9bb316bfcaa6\\refact-agent\\gui";
function resolveCommitHash() {
  const envSha = process.env.GITHUB_SHA ?? process.env.CI_COMMIT_SHA ?? process.env.VERCEL_GIT_COMMIT_SHA ?? process.env.BUILD_VCS_NUMBER;
  if (envSha && envSha.length >= 7) return envSha.slice(0, 7);
  try {
    return execSync("git rev-parse --short HEAD", {
      stdio: ["ignore", "pipe", "ignore"]
    }).toString().trim();
  } catch {
    return "unknown";
  }
}
var commitHash = resolveCommitHash();
function makeConfig(library) {
  return defineConfig(({ command, mode }) => {
    const OUT_DIR = library === "browser" ? "dist/chat" : "dist/events";
    const CONFIG = {
      // Build the webpage
      define: {
        "process.env.NODE_ENV": JSON.stringify(mode),
        __REFACT_CHAT_VERSION__: JSON.stringify({
          semver: process.env.npm_package_version,
          commit: commitHash
        }),
        "process.env.DEBUG": JSON.stringify(process.env.DEBUG),
        __REFACT_LSP_PORT__: process.env.REFACT_LSP_PORT
      },
      mode,
      build: {
        emptyOutDir: true,
        outDir: OUT_DIR,
        copyPublicDir: false,
        sourcemap: library === "browser" ? "hidden" : false,
        rollupOptions: {
          // TODO: remove when this issue is closed https://github.com/vitejs/vite/issues/15012
          onwarn(warning, defaultHandler) {
            if (warning.code === "SOURCEMAP_ERROR") {
              return;
            }
            defaultHandler(warning);
          }
        }
      },
      plugins: [react()],
      server: {
        proxy: {
          "/v1": process.env.REFACT_LSP_URL ?? "http://127.0.0.1:8001"
        }
      },
      test: {
        retry: 2,
        environment: "happy-dom",
        exclude: [...configDefaults.exclude, "tests/e2e/**", "**/*.spec.ts"],
        coverage: {
          exclude: coverageConfigDefaults.exclude.concat(
            "**/*.stories.@(js|jsx|mjs|ts|tsx)"
          )
        },
        setupFiles: ["./src/utils/test-setup.ts"],
        pool: "forks",
        poolOptions: {
          forks: {
            execArgv: ["--max-old-space-size=4096"],
            maxForks: 4,
            minForks: 1
          }
        }
      },
      css: {
        modules: {}
      }
    };
    if (command !== "serve") {
      CONFIG.mode = "production";
      CONFIG.define = {
        ...CONFIG.define,
        "process.env.NODE_ENV": "'production'"
      };
      const skipLint = process.env.REFACT_SKIP_GUI_LINT === "1";
      const skipDts = process.env.REFACT_SKIP_GUI_DTS === "1";
      if (!skipLint) {
        CONFIG.plugins?.push([
          // eslint-disable-next-line @typescript-eslint/no-unsafe-call
          eslint({
            exclude: [
              "**/node_modules/**",
              "**/virtual:/**",
              "**/src/features/Buddy/**"
            ]
          })
        ]);
      }
      if (!skipDts) {
        CONFIG.plugins?.push([
          dts({
            outDir: OUT_DIR,
            rollupTypes: true,
            insertTypesEntry: true
          })
        ]);
      }
      CONFIG.build = {
        ...CONFIG.build,
        lib: {
          entry: library === "browser" ? path.resolve(__vite_injected_original_dirname, "src/lib/index.ts") : path.resolve(__vite_injected_original_dirname, "src/events/index.ts"),
          name: "RefactChat",
          fileName: "index"
        }
      };
    }
    return CONFIG;
  });
}
var vite_config_default = makeConfig("browser");
var nodeConfig = makeConfig("node");
export {
  vite_config_default as default,
  nodeConfig
};
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsidml0ZS5jb25maWcudHMiXSwKICAic291cmNlc0NvbnRlbnQiOiBbImNvbnN0IF9fdml0ZV9pbmplY3RlZF9vcmlnaW5hbF9kaXJuYW1lID0gXCJDOlxcXFxVc2Vyc1xcXFxyYXltb25kXFxcXC5jYWNoZVxcXFxyZWZhY3RcXFxcd29ya3RyZWVzXFxcXDhhMGJjYTQwMDI3MzAyNWZcXFxcM2U5YmMyNWMtNjc4Yi00ZjA1LWJjYzUtOWJiMzE2YmZjYWE2XFxcXHJlZmFjdC1hZ2VudFxcXFxndWlcIjtjb25zdCBfX3ZpdGVfaW5qZWN0ZWRfb3JpZ2luYWxfZmlsZW5hbWUgPSBcIkM6XFxcXFVzZXJzXFxcXHJheW1vbmRcXFxcLmNhY2hlXFxcXHJlZmFjdFxcXFx3b3JrdHJlZXNcXFxcOGEwYmNhNDAwMjczMDI1ZlxcXFwzZTliYzI1Yy02NzhiLTRmMDUtYmNjNS05YmIzMTZiZmNhYTZcXFxccmVmYWN0LWFnZW50XFxcXGd1aVxcXFx2aXRlLmNvbmZpZy50c1wiO2NvbnN0IF9fdml0ZV9pbmplY3RlZF9vcmlnaW5hbF9pbXBvcnRfbWV0YV91cmwgPSBcImZpbGU6Ly8vQzovVXNlcnMvcmF5bW9uZC8uY2FjaGUvcmVmYWN0L3dvcmt0cmVlcy84YTBiY2E0MDAyNzMwMjVmLzNlOWJjMjVjLTY3OGItNGYwNS1iY2M1LTliYjMxNmJmY2FhNi9yZWZhY3QtYWdlbnQvZ3VpL3ZpdGUuY29uZmlnLnRzXCI7Ly8vIDxyZWZlcmVuY2UgdHlwZXM9XCJ2aXRlc3RcIiAvPlxuaW1wb3J0IHBhdGggZnJvbSBcInBhdGhcIjtcbmltcG9ydCB7IFBsdWdpbk9wdGlvbiwgVXNlckNvbmZpZywgZGVmaW5lQ29uZmlnIH0gZnJvbSBcInZpdGVcIjtcbmltcG9ydCByZWFjdCBmcm9tIFwiQHZpdGVqcy9wbHVnaW4tcmVhY3Qtc3djXCI7XG5pbXBvcnQgZXNsaW50IGZyb20gXCJ2aXRlLXBsdWdpbi1lc2xpbnRcIjtcblxuaW1wb3J0IHsgY29uZmlnRGVmYXVsdHMsIGNvdmVyYWdlQ29uZmlnRGVmYXVsdHMgfSBmcm9tIFwidml0ZXN0L2NvbmZpZ1wiO1xuaW1wb3J0IGR0cyBmcm9tIFwidml0ZS1wbHVnaW4tZHRzXCI7XG5cbmltcG9ydCB7IGV4ZWNTeW5jIH0gZnJvbSBcImNoaWxkX3Byb2Nlc3NcIjtcblxuZnVuY3Rpb24gcmVzb2x2ZUNvbW1pdEhhc2goKTogc3RyaW5nIHtcbiAgY29uc3QgZW52U2hhID1cbiAgICBwcm9jZXNzLmVudi5HSVRIVUJfU0hBID8/XG4gICAgcHJvY2Vzcy5lbnYuQ0lfQ09NTUlUX1NIQSA/P1xuICAgIHByb2Nlc3MuZW52LlZFUkNFTF9HSVRfQ09NTUlUX1NIQSA/P1xuICAgIHByb2Nlc3MuZW52LkJVSUxEX1ZDU19OVU1CRVI7XG5cbiAgaWYgKGVudlNoYSAmJiBlbnZTaGEubGVuZ3RoID49IDcpIHJldHVybiBlbnZTaGEuc2xpY2UoMCwgNyk7XG5cbiAgdHJ5IHtcbiAgICByZXR1cm4gZXhlY1N5bmMoXCJnaXQgcmV2LXBhcnNlIC0tc2hvcnQgSEVBRFwiLCB7XG4gICAgICBzdGRpbzogW1wiaWdub3JlXCIsIFwicGlwZVwiLCBcImlnbm9yZVwiXSxcbiAgICB9KVxuICAgICAgLnRvU3RyaW5nKClcbiAgICAgIC50cmltKCk7XG4gIH0gY2F0Y2gge1xuICAgIHJldHVybiBcInVua25vd25cIjtcbiAgfVxufVxuXG5jb25zdCBjb21taXRIYXNoID0gcmVzb2x2ZUNvbW1pdEhhc2goKTtcblxuLy8gVE9ETzogcmVtb3ZlIGV4dHJhIGNvbXBpbGUgc3RlcCB3aGVuIHZzY29kZSBjYW4gcnVuIGVzbW9kdWxlcyAgaHR0cHM6Ly9naXRodWIuY29tL21pY3Jvc29mdC92c2NvZGUvaXNzdWVzLzEzMDM2N1xuXG4vLyBodHRwczovL3ZpdGVqcy5kZXYvY29uZmlnL1xuLyoqIEB0eXBlIHtpbXBvcnQoJ3ZpdGUnKS5Vc2VyQ29uZmlnfSAqL1xuZnVuY3Rpb24gbWFrZUNvbmZpZyhsaWJyYXJ5OiBcImJyb3dzZXJcIiB8IFwibm9kZVwiKSB7XG4gIHJldHVybiBkZWZpbmVDb25maWcoKHsgY29tbWFuZCwgbW9kZSB9KSA9PiB7XG4gICAgY29uc3QgT1VUX0RJUiA9IGxpYnJhcnkgPT09IFwiYnJvd3NlclwiID8gXCJkaXN0L2NoYXRcIiA6IFwiZGlzdC9ldmVudHNcIjtcbiAgICBjb25zdCBDT05GSUc6IFVzZXJDb25maWcgPSB7XG4gICAgICAvLyBCdWlsZCB0aGUgd2VicGFnZVxuICAgICAgZGVmaW5lOiB7XG4gICAgICAgIFwicHJvY2Vzcy5lbnYuTk9ERV9FTlZcIjogSlNPTi5zdHJpbmdpZnkobW9kZSksXG4gICAgICAgIF9fUkVGQUNUX0NIQVRfVkVSU0lPTl9fOiBKU09OLnN0cmluZ2lmeSh7XG4gICAgICAgICAgc2VtdmVyOiBwcm9jZXNzLmVudi5ucG1fcGFja2FnZV92ZXJzaW9uLFxuICAgICAgICAgIGNvbW1pdDogY29tbWl0SGFzaCxcbiAgICAgICAgfSksXG4gICAgICAgIFwicHJvY2Vzcy5lbnYuREVCVUdcIjogSlNPTi5zdHJpbmdpZnkocHJvY2Vzcy5lbnYuREVCVUcpLFxuICAgICAgICBfX1JFRkFDVF9MU1BfUE9SVF9fOiBwcm9jZXNzLmVudi5SRUZBQ1RfTFNQX1BPUlQsXG4gICAgICB9LFxuICAgICAgbW9kZSxcbiAgICAgIGJ1aWxkOiB7XG4gICAgICAgIGVtcHR5T3V0RGlyOiB0cnVlLFxuICAgICAgICBvdXREaXI6IE9VVF9ESVIsXG4gICAgICAgIGNvcHlQdWJsaWNEaXI6IGZhbHNlLFxuICAgICAgICBzb3VyY2VtYXA6IGxpYnJhcnkgPT09IFwiYnJvd3NlclwiID8gXCJoaWRkZW5cIiA6IGZhbHNlLFxuICAgICAgICByb2xsdXBPcHRpb25zOiB7XG4gICAgICAgICAgLy8gVE9ETzogcmVtb3ZlIHdoZW4gdGhpcyBpc3N1ZSBpcyBjbG9zZWQgaHR0cHM6Ly9naXRodWIuY29tL3ZpdGVqcy92aXRlL2lzc3Vlcy8xNTAxMlxuICAgICAgICAgIG9ud2Fybih3YXJuaW5nLCBkZWZhdWx0SGFuZGxlcikge1xuICAgICAgICAgICAgaWYgKHdhcm5pbmcuY29kZSA9PT0gXCJTT1VSQ0VNQVBfRVJST1JcIikge1xuICAgICAgICAgICAgICByZXR1cm47XG4gICAgICAgICAgICB9XG5cbiAgICAgICAgICAgIGRlZmF1bHRIYW5kbGVyKHdhcm5pbmcpO1xuICAgICAgICAgIH0sXG4gICAgICAgIH0sXG4gICAgICB9LFxuICAgICAgcGx1Z2luczogW3JlYWN0KCldLFxuICAgICAgc2VydmVyOiB7XG4gICAgICAgIHByb3h5OiB7XG4gICAgICAgICAgXCIvdjFcIjogcHJvY2Vzcy5lbnYuUkVGQUNUX0xTUF9VUkwgPz8gXCJodHRwOi8vMTI3LjAuMC4xOjgwMDFcIixcbiAgICAgICAgfSxcbiAgICAgIH0sXG4gICAgICB0ZXN0OiB7XG4gICAgICAgIHJldHJ5OiAyLFxuICAgICAgICBlbnZpcm9ubWVudDogXCJoYXBweS1kb21cIixcbiAgICAgICAgZXhjbHVkZTogWy4uLmNvbmZpZ0RlZmF1bHRzLmV4Y2x1ZGUsIFwidGVzdHMvZTJlLyoqXCIsIFwiKiovKi5zcGVjLnRzXCJdLFxuICAgICAgICBjb3ZlcmFnZToge1xuICAgICAgICAgIGV4Y2x1ZGU6IGNvdmVyYWdlQ29uZmlnRGVmYXVsdHMuZXhjbHVkZS5jb25jYXQoXG4gICAgICAgICAgICBcIioqLyouc3Rvcmllcy5AKGpzfGpzeHxtanN8dHN8dHN4KVwiLFxuICAgICAgICAgICksXG4gICAgICAgIH0sXG4gICAgICAgIHNldHVwRmlsZXM6IFtcIi4vc3JjL3V0aWxzL3Rlc3Qtc2V0dXAudHNcIl0sXG4gICAgICAgIHBvb2w6IFwiZm9ya3NcIixcbiAgICAgICAgcG9vbE9wdGlvbnM6IHtcbiAgICAgICAgICBmb3Jrczoge1xuICAgICAgICAgICAgZXhlY0FyZ3Y6IFtcIi0tbWF4LW9sZC1zcGFjZS1zaXplPTQwOTZcIl0sXG4gICAgICAgICAgICBtYXhGb3JrczogNCxcbiAgICAgICAgICAgIG1pbkZvcmtzOiAxLFxuICAgICAgICAgIH0sXG4gICAgICAgIH0sXG4gICAgICB9LFxuICAgICAgY3NzOiB7XG4gICAgICAgIG1vZHVsZXM6IHt9LFxuICAgICAgfSxcbiAgICB9O1xuXG4gICAgaWYgKGNvbW1hbmQgIT09IFwic2VydmVcIikge1xuICAgICAgQ09ORklHLm1vZGUgPSBcInByb2R1Y3Rpb25cIjtcbiAgICAgIENPTkZJRy5kZWZpbmUgPSB7XG4gICAgICAgIC4uLkNPTkZJRy5kZWZpbmUsXG4gICAgICAgIFwicHJvY2Vzcy5lbnYuTk9ERV9FTlZcIjogXCIncHJvZHVjdGlvbidcIixcbiAgICAgIH07XG5cbiAgICAgIC8vIExvY2FsIGVzY2FwZSBoYXRjaDogc2V0IFJFRkFDVF9TS0lQX0dVSV9MSU5UPTEgLyBSRUZBQ1RfU0tJUF9HVUlfRFRTPTFcbiAgICAgIC8vIHRvIGRyb3AgdGhlIHR3byBzbG93ZXN0IHBsdWdpbnMgZnJvbSBhIHBhY2thZ2luZyBidWlsZC4gVGhlIGVtaXR0ZWRcbiAgICAgIC8vIGJ1bmRsZXMgYXJlIGJ5dGUtaWRlbnRpY2FsOyBvbmx5IHRoZSBsaW50IHBhc3MgYW5kIHRoZSByb2xsZWQtdXAgLmQudHNcbiAgICAgIC8vIGZpbGVzIGFyZSBvbWl0dGVkLCBzbyBuZXZlciB1c2UgaXQgZm9yIGEgcHVibGlzaC5cbiAgICAgIGNvbnN0IHNraXBMaW50ID0gcHJvY2Vzcy5lbnYuUkVGQUNUX1NLSVBfR1VJX0xJTlQgPT09IFwiMVwiO1xuICAgICAgY29uc3Qgc2tpcER0cyA9IHByb2Nlc3MuZW52LlJFRkFDVF9TS0lQX0dVSV9EVFMgPT09IFwiMVwiO1xuXG4gICAgICBpZiAoIXNraXBMaW50KSB7XG4gICAgICAgIENPTkZJRy5wbHVnaW5zPy5wdXNoKFtcbiAgICAgICAgICAvLyBlc2xpbnQtZGlzYWJsZS1uZXh0LWxpbmUgQHR5cGVzY3JpcHQtZXNsaW50L25vLXVuc2FmZS1jYWxsXG4gICAgICAgICAgZXNsaW50KHtcbiAgICAgICAgICAgIGV4Y2x1ZGU6IFtcbiAgICAgICAgICAgICAgXCIqKi9ub2RlX21vZHVsZXMvKipcIixcbiAgICAgICAgICAgICAgXCIqKi92aXJ0dWFsOi8qKlwiLFxuICAgICAgICAgICAgICBcIioqL3NyYy9mZWF0dXJlcy9CdWRkeS8qKlwiLFxuICAgICAgICAgICAgXSxcbiAgICAgICAgICB9KSBhcyBQbHVnaW5PcHRpb24sXG4gICAgICAgIF0pO1xuICAgICAgfVxuXG4gICAgICBpZiAoIXNraXBEdHMpIHtcbiAgICAgICAgQ09ORklHLnBsdWdpbnM/LnB1c2goW1xuICAgICAgICAgIGR0cyh7XG4gICAgICAgICAgICBvdXREaXI6IE9VVF9ESVIsXG4gICAgICAgICAgICByb2xsdXBUeXBlczogdHJ1ZSxcbiAgICAgICAgICAgIGluc2VydFR5cGVzRW50cnk6IHRydWUsXG4gICAgICAgICAgfSksXG4gICAgICAgIF0pO1xuICAgICAgfVxuXG4gICAgICBDT05GSUcuYnVpbGQgPSB7XG4gICAgICAgIC4uLkNPTkZJRy5idWlsZCxcbiAgICAgICAgbGliOiB7XG4gICAgICAgICAgZW50cnk6XG4gICAgICAgICAgICBsaWJyYXJ5ID09PSBcImJyb3dzZXJcIlxuICAgICAgICAgICAgICA/IHBhdGgucmVzb2x2ZShfX2Rpcm5hbWUsIFwic3JjL2xpYi9pbmRleC50c1wiKVxuICAgICAgICAgICAgICA6IHBhdGgucmVzb2x2ZShfX2Rpcm5hbWUsIFwic3JjL2V2ZW50cy9pbmRleC50c1wiKSxcbiAgICAgICAgICBuYW1lOiBcIlJlZmFjdENoYXRcIixcbiAgICAgICAgICBmaWxlTmFtZTogXCJpbmRleFwiLFxuICAgICAgICB9LFxuICAgICAgfTtcbiAgICB9XG5cbiAgICByZXR1cm4gQ09ORklHO1xuICB9KTtcbn1cblxuZXhwb3J0IGRlZmF1bHQgbWFrZUNvbmZpZyhcImJyb3dzZXJcIik7XG5cbmV4cG9ydCBjb25zdCBub2RlQ29uZmlnID0gbWFrZUNvbmZpZyhcIm5vZGVcIik7XG4iXSwKICAibWFwcGluZ3MiOiAiO0FBQ0EsT0FBTyxVQUFVO0FBQ2pCLFNBQW1DLG9CQUFvQjtBQUN2RCxPQUFPLFdBQVc7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
