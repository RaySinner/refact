---
title: Configure Providers (BYOK)
description: Configure BYOK providers, local runtimes, and default models in Refact.
---

Refact does not bundle a model service. Add at least one provider or local runtime before using chat, agent workflows, code completion, embeddings, or semantic search.

## Provider Setup

1. Open **Provider Setup** or **Configure Providers** in the Refact UI.
2. Choose a provider template.
3. Enter an API key, complete OAuth if the provider supports it, or set a local endpoint URL.
4. Enable the models you want Refact to use.
5. Save the provider and open **Default Models**.

Provider settings are stored locally. Provider billing, rate limits, model access, and data policies are controlled by the provider or runtime you configure.

## Current Provider Families

| Provider family | Notes |
| --- | --- |
| Anthropic | Anthropic Messages API. |
| OpenAI | Chat Completions API. |
| OpenAI Responses | OpenAI Responses API. |
| OpenAI Codex | Codex-oriented models discovered from OpenAI Responses. |
| OpenRouter | OpenRouter model catalog and OpenAI-compatible chat endpoint. |
| Ollama | Local OpenAI-compatible runtime with chat and completion support. |
| LM Studio | Local OpenAI-compatible runtime with chat and completion support. |
| vLLM | Self-hosted OpenAI-compatible runtime with chat and completion support. |
| Groq | OpenAI-compatible hosted provider. |
| DeepSeek | OpenAI-compatible hosted provider. |
| Doubao | Volcengine/Doubao OpenAI-compatible provider. |
| xAI | Chat Completions and Responses endpoints. |
| Gemini | Google Gemini OpenAI-compatible endpoints. |
| Qwen | DashScope-compatible Qwen provider. |
| Kimi | Moonshot/Kimi provider. |
| Zhipu | Zhipu/Z.ai provider. |
| MiniMax | MiniMax Anthropic-compatible endpoint. |
| GitHub Copilot | GitHub Copilot chat endpoint. |
| Custom | User-defined OpenAI-compatible endpoint. |
| Claude Code | Claude Code/Anthropic-compatible provider flow. |

## Dynamic Models And Capabilities

Refact combines provider data with a local capability registry. Depending on the provider, available models can come from:

- A provider API call.
- A provider template bundled with Refact.
- Custom model entries you add in the provider settings.

The capability registry describes context windows, tool support, agent suitability, vision support, reasoning settings, tokenizers, completion behavior, and other limits. If a model appears in a provider but Refact does not yet know its capabilities, add or adjust the custom model settings in the provider UI.

## Choose Default Models

Open **Default Models** after adding a provider. Select models for:

- Chat and agent work.
- Fast or light chat.
- Reasoning or thinking workflows.
- Buddy/background suggestions.
- Code completion.
- Embeddings for semantic search or knowledge.

Code completion requires a model source that supports completion. Ollama, LM Studio, vLLM, and custom OpenAI-compatible endpoints can provide completion when configured with compatible models.

## Local Runtimes

For Ollama, LM Studio, or vLLM, start the runtime before using Refact and make sure the configured model is available. If the runtime is on the same machine, requests stay on localhost. If you point Refact at a remote endpoint, requests go to that endpoint.

## Credentials And Privacy

Provider credentials are stored in local Refact configuration files. Refact sends prompts, code context, and tool outputs only to model providers, local endpoints, and integrations you configure for the current workflow.
