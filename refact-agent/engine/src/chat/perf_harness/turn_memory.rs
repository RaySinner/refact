use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TurnMemoryRetainedBytes {
    pub canonical_messages: usize,
    pub last_prompt_messages: usize,
    pub catalog_descriptors_and_aliases: usize,
    pub pool_vectors: usize,
}

impl TurnMemoryRetainedBytes {
    pub fn total(&self) -> usize {
        self.canonical_messages
            .saturating_add(self.last_prompt_messages)
            .saturating_add(self.catalog_descriptors_and_aliases)
            .saturating_add(self.pool_vectors)
    }

    pub(crate) fn add_assign(&mut self, other: &Self) {
        self.canonical_messages = self
            .canonical_messages
            .saturating_add(other.canonical_messages);
        self.last_prompt_messages = self
            .last_prompt_messages
            .saturating_add(other.last_prompt_messages);
        self.catalog_descriptors_and_aliases = self
            .catalog_descriptors_and_aliases
            .saturating_add(other.catalog_descriptors_and_aliases);
        self.pool_vectors = self.pool_vectors.saturating_add(other.pool_vectors);
    }
}

impl crate::chat::types::ChatSession {
    pub fn retained_bytes_for_turn_memory(&self) -> TurnMemoryRetainedBytes {
        let canonical_messages = self
            .messages
            .iter()
            .map(|message| {
                serde_json::to_vec(message)
                    .map(|encoded| encoded.len())
                    .unwrap_or_default()
            })
            .sum();
        let last_prompt_messages = self
            .last_prompt_messages
            .iter()
            .map(|message| {
                serde_json::to_vec(message)
                    .map(|encoded| encoded.len())
                    .unwrap_or_default()
            })
            .sum();
        let catalog_descriptors_and_aliases = self
            .tool_catalog
            .as_ref()
            .map(|catalog| {
                catalog
                    .index
                    .tools
                    .iter()
                    .map(|descriptor| {
                        serde_json::to_vec(descriptor)
                            .map(|encoded| encoded.len())
                            .unwrap_or_default()
                    })
                    .sum::<usize>()
                    .saturating_add(
                        catalog
                            .index
                            .tools
                            .iter()
                            .map(|descriptor| descriptor.name.len().saturating_mul(2))
                            .sum::<usize>(),
                    )
            })
            .unwrap_or_default();
        let pool_vectors = self
            .turn_tool_pool
            .as_ref()
            .map(|_| {
                self.tool_catalog
                    .as_ref()
                    .map(|catalog| {
                        std::mem::size_of::<refact_runtime_api::TurnToolPool>().saturating_add(
                            catalog
                                .index
                                .tools
                                .len()
                                .saturating_mul(std::mem::size_of::<usize>().saturating_mul(2)),
                        )
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        TurnMemoryRetainedBytes {
            canonical_messages,
            last_prompt_messages,
            catalog_descriptors_and_aliases,
            pool_vectors,
        }
    }
}

pub fn aggregate_turn_memory_retained_bytes(
    sessions: impl IntoIterator<Item = TurnMemoryRetainedBytes>,
) -> TurnMemoryRetainedBytes {
    sessions.into_iter().fold(
        TurnMemoryRetainedBytes {
            canonical_messages: 0,
            last_prompt_messages: 0,
            catalog_descriptors_and_aliases: 0,
            pool_vectors: 0,
        },
        |mut total, retained| {
            total.add_assign(&retained);
            total
        },
    )
}
