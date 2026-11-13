use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::Duration;
use async_trait::async_trait;
use refact_core::ast_types::{AstDefinition, SymbolType};
use refact_chat_api::{attach_tool_enrichment, ToolEnrichment, ToolEnrichmentKind, ToolEnrichmentProvenance, ToolEnrichmentReference};
use serde_json::Value;
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_runtime_api::GlobalContextCtx;
use refact_runtime_api::scope_utils::{
    format_scope_notices, remap_context_file_for_execution_scope_for_model_context,
};
use refact_runtime_api::tool_ctx::ToolCtx;
use crate::tools::tools_description::{
    Tool, ToolDesc, ToolSource, ToolSourceType, json_schema_from_params,
};
use refact_core::chat_types::{ChatMessage, ChatContent, ContextEnum, ContextFile};
use refact_core::knowledge_index::{KnowledgeCard, KnowledgeIndex};
use refact_postprocessing::pp_command_output::OutputFilter;
use crate::tools::native_enrichment::{workspace_roots, NativeReferences};
use refact_worktrees::scope::ExecutionScope;
use regex::Regex;

pub const MAX_SYMBOLS: usize = 16;
const DEFS_LIMIT: usize = 20;
const HIERARCHY_LIMIT: usize = 8;
const RELATED_MEMORIES_LIMIT: usize = 8;

pub static IDENTIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z_][a-zA-Z0-9_:]{1,100}$").unwrap());

pub struct ToolAstDefinition {
    pub config_path: String,
}

fn is_type_definition(def: &AstDefinition) -> bool {
    def.symbol_type == SymbolType::StructDeclaration || !def.this_is_a_class.is_empty()
}

pub async fn type_hierarchy_sections(
    service: &Arc<refact_codegraph::CodeGraphService>,
    defs: &[Arc<AstDefinition>],
    abort_flag: &Arc<AtomicBool>,
) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    let mut sections = Vec::new();
    for def in defs {
        if abort_flag.load(Ordering::SeqCst) {
            break;
        }
        if sections.len() >= HIERARCHY_LIMIT {
            break;
        }
        if !is_type_definition(def) {
            continue;
        }
        let name = def.name();
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }
        let hierarchy = service.type_hierarchy(&name).await?;
        if hierarchy.trim().is_empty() {
            continue;
        }
        sections.push(format!(
            "Inheritance for `{}`:\n{}",
            name,
            hierarchy.trim_end()
        ));
    }
    Ok(sections)
}

pub fn related_memory_entities(symbols_str: &str) -> Vec<String> {
    let mut entities = Vec::new();
    for raw in symbols_str.split(',') {
        let symbol = raw.trim();
        if symbol.is_empty() {
            continue;
        }
        let symbol = symbol.replace('.', "::");
        if let Some(last) = symbol.split("::").last() {
            if !last.is_empty() {
                entities.push(last.to_string());
            }
        }
        entities.push(symbol);
    }
    entities.sort();
    entities.dedup();
    entities.retain(|entity| IDENTIFIER_RE.is_match(entity));
    entities
}

pub async fn compute_related_memories(
    index: &Arc<AMutex<refact_core::knowledge_index::KnowledgeIndex>>,
    mut files: Vec<String>,
    symbols_str: &str,
    abort_flag: &Arc<AtomicBool>,
) -> Vec<KnowledgeCard> {
    files.sort();
    files.dedup();
    let index = index.clone();
    let Some(mut cards) = knowledge_index_lookup(&index, abort_flag, |index| {
        index.related_for_files(&files, RELATED_MEMORIES_LIMIT)
    })
    .await
    else {
        return Vec::new();
    };
    if cards.is_empty() {
        let Some(found) = knowledge_index_lookup(&index, abort_flag, |index| {
            index.related_for_related_files(&files, RELATED_MEMORIES_LIMIT)
        })
        .await
        else {
            return Vec::new();
        };
        cards = found;
    }
    if cards.is_empty() {
        let entities = related_memory_entities(symbols_str);
        if !entities.is_empty() {
            let Some(found) = knowledge_index_lookup(&index, abort_flag, |index| {
                index.related_for_entities(&entities, RELATED_MEMORIES_LIMIT)
            })
            .await
            else {
                return Vec::new();
            };
            cards = found;
            if cards.is_empty() {
                let Some(found) = knowledge_index_lookup(&index, abort_flag, |index| {
                    index.related_for_related_entities(&entities, RELATED_MEMORIES_LIMIT)
                })
                .await
                else {
                    return Vec::new();
                };
                cards = found;
            }
        }
    }
    cards
}

async fn knowledge_index_lookup<T>(
    index: &AMutex<KnowledgeIndex>,
    abort_flag: &AtomicBool,
    lookup: impl FnOnce(&KnowledgeIndex) -> T,
) -> Option<T> {
    if abort_flag.load(Ordering::SeqCst) {
        return None;
    }
    let lock = index.lock();
    tokio::pin!(lock);
    loop {
        tokio::select! {
            guard = &mut lock => {
                if abort_flag.load(Ordering::SeqCst) {
                    return None;
                }
                return Some(lookup(&guard));
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if abort_flag.load(Ordering::SeqCst) {
                    return None;
                }
            }
        }
    }
}

fn workspace_relative_path(path: &Path, roots: &[PathBuf]) -> Option<String> {
    let relative = roots.iter().find_map(|root| path.strip_prefix(root).ok())?;
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let relative = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => part.to_str(),
            std::path::Component::CurDir => None,
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    (!relative.is_empty()).then_some(relative)
}

async fn related_memory_references(
    tool: &ToolCtx,
    cards: Vec<KnowledgeCard>,
    roots: &[PathBuf],
    execution_scope: Option<&ExecutionScope>,
    abort_flag: &Arc<AtomicBool>,
) -> Vec<ToolEnrichmentReference> {
    let mut references = Vec::new();
    for card in cards.into_iter().take(RELATED_MEMORIES_LIMIT) {
        if abort_flag.load(Ordering::SeqCst) {
            break;
        }
        let context_file = ContextFile {
            file_name: card.file_path.to_string_lossy().to_string(),
            file_content: String::new(),
            line1: 0,
            line2: 0,
            file_rev: None,
            symbols: vec![],
            gradient_type: 0,
            usefulness: 0.0,
            skip_pp: true,
        };
        let Ok(Some((context_file, _))) = remap_context_file_for_execution_scope_for_model_context(
            tool,
            execution_scope,
            context_file,
        )
        .await
        else {
            continue;
        };
        let Some(target) = workspace_relative_path(Path::new(&context_file.file_name), roots)
        else {
            continue;
        };
        let mut reference = ToolEnrichmentReference::new(ToolEnrichmentKind::Path, target);
        reference.provenance = ToolEnrichmentProvenance::Native;
        reference.label = Some(card.title);
        reference.status = Some("related_memory".to_string());
        reference.source = Some("search_symbol_definition".to_string());
        references.push(reference);
    }
    references
}

pub async fn symbol_def_via_codegraph(
    gcx: Arc<refact_runtime_api::global_context::GlobalContext>,
    service: Arc<refact_codegraph::CodeGraphService>,
    symbols: &[String],
    symbols_str: &str,
    tool_call_id: &String,
    execution_scope: Option<&ExecutionScope>,
    abort_flag: Arc<AtomicBool>,
) -> Result<(bool, Vec<ContextEnum>), String> {
    let mut corrections = false;
    let mut all_messages = Vec::new();
    let mut all_context_files = Vec::new();
    let mut all_notices: Vec<String> = Vec::new();
    let roots = workspace_roots(&gcx, execution_scope);
    let mut references = NativeReferences::new();

    for symbol in symbols {
        if abort_flag.load(Ordering::SeqCst) {
            all_messages.push("⚠️ Aborted before all symbols were processed.".to_string());
            break;
        }

        let defs = service.definitions(symbol).await?;
        if defs.is_empty() {
            references.add_symbol(symbol, "not_found", "search_symbol_definition");
            corrections = true;
            let fuzzy = service.definition_paths_fuzzy(symbol, 20).await?;
            if fuzzy.is_empty() {
                let counters = service.fetch_counters().await?;
                all_messages.push(format!(
                    "For symbol `{}`:\n⚠️ No definitions found ({} total in codegraph). 💡 Check spelling or use search_pattern() to find text\n",
                    symbol, counters.counter_defs
                ));
            } else {
                let mut msg = format!(
                    "For symbol `{}`:\n⚠️ No exact match. 💡 Similar definitions found:\n",
                    symbol
                );
                for line in fuzzy {
                    msg.push_str(&format!("{}\n", line));
                }
                all_messages.push(msg);
            }
            continue;
        }

        let mut context_files = Vec::new();
        for res in defs.iter() {
            let context_file = ContextFile {
                file_name: res.cpath.clone(),
                file_content: "".to_string(),
                line1: res.full_line1(),
                line2: res.full_line2(),
                file_rev: None,
                symbols: vec![res.path_drop0()],
                gradient_type: 5,
                usefulness: 100.0,
                skip_pp: false,
            };
            if let Some((context_file, notices)) =
                remap_context_file_for_execution_scope_for_model_context(
                    &gcx.tool_ctx(gcx.clone()),
                    execution_scope,
                    context_file,
                )
                .await?
            {
                context_files.push(context_file);
                all_notices.extend(notices);
                if context_files.len() >= DEFS_LIMIT {
                    break;
                }
            }
        }

        if context_files.is_empty() {
            references.add_symbol(symbol, "suppressed", "search_symbol_definition");
            corrections = true;
            all_messages.push(format!(
                "For symbol `{}`:\n⚠️ Definitions found only outside the active worktree and were suppressed. 💡 Use search_pattern() within the worktree\n",
                symbol
            ));
            continue;
        }

        references.add_symbol(symbol, "found", "search_symbol_definition");
        for context_file in &context_files {
            references.add_context_file(
                context_file,
                &roots,
                "definition",
                "search_symbol_definition",
            );
        }

        let file_paths = context_files
            .iter()
            .map(|cf| cf.file_name.clone())
            .collect::<Vec<_>>();
        let short_file_paths =
            refact_runtime_api::files_correction::shortify_paths(gcx.clone(), &file_paths).await;
        let mut tool_message = format!("Definitions for `{}`:\n", symbol);
        for (cf, short_path) in context_files.iter().zip(short_file_paths.iter()) {
            let symbol_path = cf.symbols.get(0).cloned().unwrap_or_default();
            tool_message.push_str(&format!(
                "{} defined at {}:{}-{}\n",
                symbol_path, short_path, cf.line1, cf.line2
            ));
        }
        if abort_flag.load(Ordering::SeqCst) {
            all_messages.push(tool_message);
            all_context_files.extend(context_files.into_iter().map(ContextEnum::ContextFile));
            all_messages.push("⚠️ Aborted before type hierarchy was computed.".to_string());
            break;
        }
        let hierarchy_sections = type_hierarchy_sections(&service, &defs, &abort_flag).await?;
        if !hierarchy_sections.is_empty() {
            tool_message.push_str("Inheritance:\n");
            tool_message.push_str(&hierarchy_sections.join("\n\n"));
            tool_message.push('\n');
        }
        if defs.len() > context_files.len() {
            references.mark_truncated();
            tool_message.push_str(&format!(
                "⚠️ {} more definitions not shown (limit: {}). 💡 Use more specific symbol name\n",
                defs.len() - context_files.len(),
                DEFS_LIMIT
            ));
        }
        all_messages.push(tool_message);
        all_context_files.extend(context_files.into_iter().map(ContextEnum::ContextFile));
    }

    let files: Vec<String> = all_context_files
        .iter()
        .filter_map(|c| match c {
            ContextEnum::ContextFile(cf) => Some(cf.file_name.clone()),
            _ => None,
        })
        .collect();
    let related_memories = if abort_flag.load(Ordering::SeqCst) {
        Vec::new()
    } else {
        compute_related_memories(&gcx.knowledge_index, files, symbols_str, &abort_flag).await
    };
    let related_references = if abort_flag.load(Ordering::SeqCst) {
        Vec::new()
    } else {
        related_memory_references(
            &gcx.tool_ctx(gcx.clone()),
            related_memories,
            &roots,
            execution_scope,
            &abort_flag,
        )
        .await
    };

    let notices_section = format_scope_notices(&all_notices);
    let mut tool_message = ChatMessage {
        role: "tool".to_string(),
        content: ChatContent::SimpleText(format!("{}{}", all_messages.join("\n"), notices_section)),
        tool_calls: None,
        tool_call_id: tool_call_id.clone(),
        output_filter: Some(OutputFilter::no_limits()),
        ..Default::default()
    };
    if !related_references.is_empty() {
        attach_tool_enrichment(
            &mut tool_message,
            ToolEnrichment {
                references: related_references,
                ..Default::default()
            },
        );
    }
    references.attach(&mut tool_message);
    all_context_files.push(ContextEnum::ChatMessage(tool_message));

    Ok((corrections, all_context_files))
}

#[async_trait]
impl Tool for ToolAstDefinition {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let symbols_str = match args.get("symbols") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => return Err(format!("argument `symbols` is not a string: {:?}", v)),
            None => return Err("argument `symbols` is missing".to_string()),
        };

        let raw_symbol_count = symbols_str
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .count();
        if raw_symbol_count > MAX_SYMBOLS {
            return Err(format!(
                "⚠️ Too many symbols requested ({}). 💡 Pass at most {} comma-separated symbols per call.",
                raw_symbol_count, MAX_SYMBOLS
            ));
        }

        let symbols: Vec<String> = symbols_str
            .split(',')
            .map(|s| s.trim().replace('.', "::"))
            .filter(|s| !s.is_empty())
            .collect();

        if symbols.is_empty() {
            return Err("No valid symbols provided".to_string());
        }

        let (gcx, tool, execution_scope, abort_flag) = {
            let cgcx = ccx.lock().await;
            (
                cgcx.app.gcx.clone(),
                cgcx.tool.clone(),
                cgcx.execution_scope.clone(),
                cgcx.abort_flag.clone(),
            )
        };

        let codegraph_opt = tool.codeintel.codegraph.lock().await.clone();
        match codegraph_opt {
            Some(service) => {
                symbol_def_via_codegraph(
                    gcx.clone(),
                    service,
                    &symbols,
                    &symbols_str,
                    tool_call_id,
                    execution_scope.as_ref(),
                    abort_flag,
                )
                .await
            }
            None => Err("codegraph is not available".to_string()),
        }
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "search_symbol_definition".to_string(),
            display_name: "Definition".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Find definition of a symbol in the project using the codegraph".to_string(),
            input_schema: json_schema_from_params(&[("symbols", "string", "Comma-separated list of symbols to search for (functions, methods, classes, type aliases). No spaces allowed in symbol names.")], &["symbols"]),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec!["codegraph".to_string()]
    }
}
