---
title: Supported Models
description: How Refact discovers models and applies capability metadata.
---

Refact does not maintain a static public list of every usable model. Model availability changes frequently, so Refact discovers models from configured providers and combines them with local capability metadata in the engine.

## Supported Provider Registry

Refact currently includes provider support for:

| Provider | Model source |
| --- | --- |
| Anthropic | Anthropic Messages API and provider model metadata. |
| OpenAI | OpenAI Chat Completions. |
| OpenAI Responses | OpenAI Responses. |
| OpenAI Codex | Codex-oriented OpenAI Responses models. |
| OpenRouter | OpenRouter catalog and OpenAI-compatible endpoints. |
| Ollama | Local runtime model list and custom settings. |
| LM Studio | Local runtime model list and custom settings. |
| vLLM | Self-hosted OpenAI-compatible endpoints and custom settings. |
| Groq | Groq OpenAI-compatible endpoints. |
| DeepSeek | DeepSeek OpenAI-compatible endpoints. |
| Doubao | Volcengine/Doubao endpoints. |
| xAI | xAI Chat Completions and Responses endpoints. |
| Gemini | Google Gemini OpenAI-compatible endpoints. |
| Qwen | DashScope-compatible endpoints. |
| Kimi | Moonshot/Kimi endpoints. |
| Zhipu | Zhipu/Z.ai endpoints. |
| MiniMax | MiniMax Anthropic-compatible endpoint. |
| GitHub Copilot | GitHub Copilot chat endpoint. |
| Custom | User-defined OpenAI-compatible endpoints. |
| Claude Code | Claude Code/Anthropic-compatible provider flow. |

## How Refact Decides What A Model Can Do

Refact combines provider model discovery with capability metadata. That metadata can describe:

- Context window size.
- Tool and agent support.
- Vision or multimodal support.
- Reasoning and thinking options.
- Tokenizer and scratchpad behavior.
- Completion support.
- Embedding support.
- Provider-specific request format and endpoint details.

The UI uses these capabilities when you choose default chat, reasoning, completion, and embedding models.

## Dynamic Discovery Instead Of Fixed Claims

Some providers expose model lists through an API. Others use bundled provider templates, custom entries, or local runtime responses. A model can be usable even if it is not named in this documentation, provided the provider accepts it and Refact has enough capability information to route requests safely.

If a provider adds a new model before Refact has bundled metadata for it, configure a custom model entry in the provider settings. Include the context size, tokenizer, tool support, completion support, and other settings relevant to your workflow.

## Local Models

For Ollama, LM Studio, and vLLM:

1. Install or download the model in the runtime.
2. Start the runtime and confirm the endpoint is reachable.
3. Add the runtime provider in Refact.
4. Enable the model and choose it in **Default Models**.

Local runtimes can be used for chat, agents, completion, or embeddings when the runtime and model support the needed capability.

## Pricing And Usage

Refact does not sell model access. Any cost shown in the UI is a local estimate based on provider pricing metadata where available. Actual billing, quotas, model access, rate limits, and retention policies are handled by the provider or runtime you configure.
