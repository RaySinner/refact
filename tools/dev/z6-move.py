"""Z-6 harvest: integr_cmdline + integr_cmdline_service -> refact-runtime-api.

Prod bodies move to the crate with rewritten imports; the original #[cfg(test)]
blocks stay in the engine shim verbatim, plus the imports the test blocks used to
inherit from the parent module's private `use` list (a glob `pub use` does not
forward those).
"""
import os
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
SRC = os.path.join(ROOT, "refact-agent", "engine", "src", "integrations")
CRATE = os.path.join(ROOT, "refact-agent", "engine", "crates", "refact-runtime-api", "src", "integrations")

APPLY = "--apply" in sys.argv
MARKER = "#[cfg(test)]\nmod tests {"


def read(p):
    return open(p, encoding="utf-8", newline="").read()


def write(p, text):
    with open(p, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)


def subn(text, old, new, n, label=""):
    got = text.count(old)
    if got != n:
        raise SystemExit(f"[{label}] EXPECTED {n} OCCURRENCES, FOUND {got}:\n{old[:300]}")
    return text.replace(old, new)


def sub1(text, old, new, label=""):
    return subn(text, old, new, 1, label)


CMD_IMPORTS_OLD = """use crate::at_commands::at_commands::AtCommandsContext;
use crate::call_validation::{ChatContent, ChatMessage, ContextEnum};
use crate::custom_error::YamlError;
use refact_exec::command_policy::{CommandKind, CommandPolicyInput, ExecSource};
use refact_runtime_api::command_policy::build_exec_request_for_gcx as build_exec_request;
use crate::exec::{
    generate_short_description, sanitize_short_description, ExecMode, ExecOutputStream,
    ExecOwnerMeta, ExecProcessSnapshot, ExecRawOutput, ExecReadResult, ExecStatus,
};
use crate::global_context::GlobalContext;
use crate::integrations::integr_abstract::{
    IntegrationCommon, IntegrationConfirmation, IntegrationTrait,
};
use crate::integrations::utils::{
    deserialize_str_to_num, deserialize_str_to_opt_num, serialize_num_to_str,
    serialize_opt_num_to_str,
};
use crate::postprocessing::pp_command_output::{output_mini_postprocessing, OutputFilter};
use refact_runtime_api::Tool;
use refact_tool_api::{ToolDesc, ToolSource, ToolSourceType};
use refact_buddy_core::user_action::UserAction;
"""

CMD_IMPORTS_NEW = """use crate::at_commands_ctx::AtCommandsContext;
use crate::command_policy::build_exec_request_for_gcx as build_exec_request;
use crate::files_correction::get_project_dirs;
use crate::gcx_helpers::{
    filter_path_enrichment_for_model_context, tool_enrichment_from_path_references,
};
use crate::global_context::GlobalContext;
use crate::integr_abstract::IntegrationTrait;
use crate::integrations::setting_up_integrations::get_vars_for_replacements;
use crate::privacy::load_privacy_if_needed;
use crate::Tool;
use refact_buddy_core::user_action::UserAction;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_core::custom_error::YamlError;
use refact_exec::command_policy::{CommandKind, CommandPolicyInput, ExecSource};
use refact_exec::{
    generate_short_description, sanitize_short_description, ExecMode, ExecOutputStream,
    ExecOwnerMeta, ExecProcessSnapshot, ExecRawOutput, ExecReadResult, ExecStatus,
};
use refact_integrations::integr_abstract::IntegrationCommon;
use refact_integrations::utils::{
    deserialize_str_to_num, deserialize_str_to_opt_num, serialize_num_to_str,
    serialize_opt_num_to_str,
};
use refact_postprocessing::pp_command_output::{output_mini_postprocessing, OutputFilter};
use refact_tool_api::IntegrationConfirmation;
use refact_tool_api::{ToolDesc, ToolSource, ToolSourceType};
"""

SVC_IMPORTS_OLD = """use crate::at_commands::at_commands::AtCommandsContext;
use crate::call_validation::{ChatContent, ChatMessage, ContextEnum};
use crate::exec::{
    generate_short_description, sanitize_short_description, ExecMode, ExecOutputChunk,
    ExecOutputStream, ExecOwnerMeta, ExecReadResult, ExecReadinessProbe, ExecServiceLookup,
    ExecSpawnRequest, ExecStatus,
};
use crate::exec::types::normalize_workspace_path;
use crate::global_context::GlobalContext;
use crate::integrations::integr_abstract::{
    IntegrationCommon, IntegrationConfirmation, IntegrationTrait,
};
use crate::integrations::integr_cmdline::{format_output, replace_args, CmdlineToolConfig};
use crate::postprocessing::pp_command_output::{output_mini_postprocessing, OutputFilter};
use refact_runtime_api::Tool;
use refact_tool_api::{ToolDesc, ToolSource, ToolSourceType};
"""

SVC_IMPORTS_NEW = """use super::cmdline::{format_output, replace_args, CmdlineToolConfig};
use crate::at_commands_ctx::AtCommandsContext;
use crate::files_correction::{get_active_project_path, get_project_dirs};
use crate::global_context::GlobalContext;
use crate::integr_abstract::IntegrationTrait;
use crate::integrations::setting_up_integrations::get_vars_for_replacements;
use crate::Tool;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_exec::types::normalize_workspace_path;
use refact_exec::{
    generate_short_description, sanitize_short_description, ExecMode, ExecOutputChunk,
    ExecOutputStream, ExecOwnerMeta, ExecProcessSnapshot, ExecReadResult, ExecReadinessProbe,
    ExecServiceLookup, ExecSpawnRequest, ExecStatus,
};
use refact_integrations::integr_abstract::IntegrationCommon;
use refact_postprocessing::pp_command_output::{output_mini_postprocessing, OutputFilter};
use refact_tool_api::{IntegrationConfirmation, ToolDesc, ToolSource, ToolSourceType};
"""

CMD_SHIM_PRELUDE = """#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use tokio::sync::Mutex as AMutex;

#[cfg(test)]
use crate::at_commands::at_commands::AtCommandsContext;
#[cfg(test)]
use crate::call_validation::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
use crate::exec::{ExecMode, ExecProcessId, ExecProcessMeta, ExecProcessSnapshot, ExecStatus};
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use crate::integrations::integr_abstract::{IntegrationCommon, IntegrationConfirmation};
#[cfg(test)]
use crate::postprocessing::pp_command_output::OutputFilter;
"""

SVC_SHIM_PRELUDE = """#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;

#[cfg(test)]
use crate::at_commands::at_commands::AtCommandsContext;
#[cfg(test)]
use crate::call_validation::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
use crate::exec::{ExecServiceLookup, ExecStatus};
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use crate::integrations::integr_cmdline::CmdlineParam;
#[cfg(test)]
use crate::integrations::integr_cmdline::CmdlineToolConfig;
"""

# Call-site rewrites, applied BEFORE the import block is swapped (the original
# import block also contains some of these spellings). `(` makes each pattern
# unambiguous against the `use …;` line.
CMD_CALL_SITES = [
    ("crate::integrations::setting_up_integrations::get_vars_for_replacements(",
     "get_vars_for_replacements("),
    ("refact_runtime_api::files_correction::get_project_dirs(", "get_project_dirs("),
    ("refact_runtime_api::gcx_helpers::filter_path_enrichment_for_model_context(",
     "filter_path_enrichment_for_model_context("),
    ("refact_runtime_api::gcx_helpers::tool_enrichment_from_path_references(",
     "tool_enrichment_from_path_references("),
    ("crate::privacy::load_privacy_if_needed(", "load_privacy_if_needed("),
    ("crate::exec::path_enrichment::bounded_diagnostic_output(",
     "refact_exec::path_enrichment::bounded_diagnostic_output("),
    ("crate::exec::path_enrichment::collect_async_with_truncation(",
     "refact_exec::path_enrichment::collect_async_with_truncation("),
]

SVC_CALL_SITES = [
    ("crate::integrations::setting_up_integrations::get_vars_for_replacements(",
     "get_vars_for_replacements("),
    ("refact_runtime_api::files_correction::get_project_dirs(", "get_project_dirs("),
    ("refact_runtime_api::files_correction::get_active_project_path(", "get_active_project_path("),
]

SVC_MULTI = {
    "refact_runtime_api::files_correction::get_active_project_path(": 2,
}

CMD_PUB = [
    "fn append_status_line(", "fn tool_failed_for_status(", "fn exec_extra(",
    "fn powershell_escape(", "fn resolve_cmdline_workdir(", "fn collect_exec_output(",
    "fn collect_foreground_output(", "fn exec_status_label(", "fn exec_exit_code(",
    "fn cmdline_short_description(", "fn _parse_command_args(",
]


def apply_call_sites(prod, sites):
    for old, new in sites:
        want = SVC_MULTI.get(old, 1)
        prod = subn(prod, old, new, want, old)
    return prod


def rewrite_cmdline(prod):
    prod = apply_call_sites(prod, CMD_CALL_SITES)
    prod = sub1(prod, "&crate::exec::ExecRegistry,", "&refact_exec::ExecRegistry,", "cmdline.registry")
    for name in CMD_PUB:
        prod = sub1(prod, name, "pub " + name, "cmdline.pub " + name)
    prod = sub1(prod, CMD_IMPORTS_OLD, CMD_IMPORTS_NEW, "cmdline.imports")
    prod = sub1(prod, "refact_runtime_api::Tool", "crate::Tool", "cmdline.tool_trait")
    return prod


def rewrite_service(prod):
    prod = apply_call_sites(prod, SVC_CALL_SITES)
    prod = subn(prod, "&crate::exec::ExecRegistry,", "&refact_exec::ExecRegistry,", 3, "svc.registry")
    prod = sub1(prod, "snapshot: &crate::exec::ExecProcessSnapshot",
                "snapshot: &ExecProcessSnapshot", "svc.snapshot_ref")
    prod = subn(prod, "Option<crate::exec::ExecProcessSnapshot>",
                "Option<ExecProcessSnapshot>", 2, "svc.snapshot_opt")
    prod = sub1(prod, "fn parse_service_tool_args(", "pub fn parse_service_tool_args(", "svc.parse_args")
    prod = sub1(prod, SVC_IMPORTS_OLD, SVC_IMPORTS_NEW, "svc.imports")
    return prod


def main():
    jobs = [
        ("integr_cmdline.rs", "cmdline.rs", "refact_runtime_api::integrations::cmdline",
         rewrite_cmdline, CMD_SHIM_PRELUDE),
        ("integr_cmdline_service.rs", "cmdline_service.rs",
         "refact_runtime_api::integrations::cmdline_service", rewrite_service, SVC_SHIM_PRELUDE),
    ]
    for engine_name, crate_name, module_path, rewrite, shim_prelude in jobs:
        path = os.path.join(SRC, engine_name)
        text = read(path).replace("\r\n", "\n")
        i = text.index(MARKER)
        prod, tests = text[:i], text[i:]
        crate_body = rewrite(prod)
        shim = f"pub use {module_path}::*;\n\n{shim_prelude}\n{tests}"
        print(f"=== {engine_name}: prod {len(prod.splitlines())}L -> "
              f"crates/refact-runtime-api/src/integrations/{crate_name} ({len(crate_body.splitlines())}L)")
        print(f"    tests {len(tests.splitlines())}L stay in the engine shim verbatim")
        for ln in crate_body.splitlines():
            if "crate::exec" in ln or "crate::call_validation" in ln or "crate::postprocessing" in ln \
                    or "crate::at_commands" in ln or "crate::custom_error" in ln \
                    or "refact_runtime_api::" in ln or "crate::integrations::integr_cmdline" in ln:
                print("    LEFTOVER:", ln.strip())
        if APPLY:
            write(os.path.join(CRATE, crate_name), crate_body)
            write(path, shim)
    if APPLY:
        print("APPLIED")


main()
