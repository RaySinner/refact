use std::collections::HashMap;
use std::sync::Arc;

use crate::at_commands_ctx::AtCommand;
use crate::at_commands_impl::at_ast_definition::AtAstDefinition;
use crate::at_commands_impl::at_file::AtFile;
use crate::at_commands_impl::at_knowledge::AtLoadKnowledge;
use crate::at_commands_impl::at_search::AtSearch;
use crate::at_commands_impl::at_tree::AtTree;
use crate::at_commands_impl::at_web::AtWeb;

pub fn engine_at_commands_dict() -> HashMap<String, Arc<dyn AtCommand + Send>> {
    HashMap::from([
        (
            "@file".to_string(),
            Arc::new(AtFile::new()) as Arc<dyn AtCommand + Send>,
        ),
        (
            "@definition".to_string(),
            Arc::new(AtAstDefinition::new()) as Arc<dyn AtCommand + Send>,
        ),
        (
            "@tree".to_string(),
            Arc::new(AtTree::new()) as Arc<dyn AtCommand + Send>,
        ),
        (
            "@web".to_string(),
            Arc::new(AtWeb::new()) as Arc<dyn AtCommand + Send>,
        ),
        (
            "@search".to_string(),
            Arc::new(AtSearch::new()) as Arc<dyn AtCommand + Send>,
        ),
        (
            "@knowledge-load".to_string(),
            Arc::new(AtLoadKnowledge::new()) as Arc<dyn AtCommand + Send>,
        ),
    ])
}
