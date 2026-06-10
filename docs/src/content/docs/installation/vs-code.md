---
title: VS Code
description: Install Refact for VS Code and complete local-first setup.
---

## Install

Install Refact from the [Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=smallcloud.codify). You can also install a local extension build if you are developing Refact from source.

## Open Refact

After installation, open the Refact activity bar view or command palette entry. The extension starts the local `refact-lsp` engine and opens the Refact UI inside VS Code.

## Complete First-Run Setup

1. Open **Provider Setup**.
2. Add a hosted provider, local runtime, or custom endpoint.
3. Enter the provider key, complete OAuth if required, or confirm the local endpoint URL.
4. Enable the models you want to use.
5. Open **Default Models** and choose defaults for chat, agent work, reasoning, completion, and embeddings as needed.

Refact works with BYOK providers such as Anthropic, OpenAI, OpenRouter, Groq, DeepSeek, Gemini, xAI, Qwen, Kimi, Zhipu, MiniMax, GitHub Copilot, Claude Code, and custom OpenAI-compatible endpoints. Local runtimes include Ollama, LM Studio, and vLLM.

## Start Using Refact In VS Code

- Open Chat and ask about the current workspace.
- Switch to an agent mode for multi-step tasks that can search, edit, run commands, and use integrations.
- Enable inline completion with a completion-capable model source.
- Add project knowledge, task context, and integration settings as your workflow grows.

## Local Engine Notes

The VS Code extension communicates with the engine over localhost. Project trajectories, task state, knowledge, and provider settings stay in local Refact directories unless you configure providers or integrations that need network access.

A hosted Refact login, Refact-issued model key, or separate backend deployment is not required.
