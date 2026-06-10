---
title: Quickstart
description: Install Refact, configure a provider, and start chat, agent, and completion workflows.
---

Refact runs a local engine in your IDE and sends model requests only to providers or runtimes that you configure. A hosted Refact account, plan, or model credit wallet is not required.

## 1. Install The IDE Extension

Choose the IDE you use most:

- [VS Code](/installation/vs-code/)
- [JetBrains IDEs](/installation/jetbrains/)

After installation, open the Refact sidebar or tool window. The extension starts the local `refact-lsp` engine automatically.

## 2. Add A Provider Or Local Runtime

Open **Provider Setup** and add at least one model source:

- Hosted BYOK providers: Anthropic, OpenAI, OpenAI Responses, OpenAI Codex, OpenRouter, Groq, DeepSeek, Doubao, xAI, Gemini, Qwen, Kimi, Zhipu, MiniMax, GitHub Copilot, Claude Code, or a custom OpenAI-compatible endpoint.
- Local or self-managed runtimes: Ollama, LM Studio, vLLM, or a compatible endpoint you run yourself.

Provider keys, OAuth tokens, endpoints, and enabled models are stored in your local Refact configuration. Billing, quotas, model availability, and data retention are controlled by the provider or runtime you choose.

## 3. Choose Default Models

Open **Default Models** and select the models Refact should use for common roles:

- Chat and agent work.
- Fast or light chat.
- Reasoning or thinking workflows, when your model supports them.
- Buddy/background suggestions, if enabled.
- Code completion, if your provider or local runtime supports completion.
- Embeddings, when you use semantic search or knowledge features.

You can change these defaults later without reinstalling Refact.

## 4. Start A Chat

Open Refact Chat in your IDE and ask a question about the current project. Attach files or selected code when useful. Refact can combine your message with local project context and enabled tools.

## 5. Try Agent Mode

Switch to an agent-capable mode when you want Refact to search, edit, run commands, inspect integrations, or iterate on a task. Tool confirmations and rollback features are available for workflows that change files.

## 6. Enable Code Completion

Use a completion-capable provider or local runtime for inline suggestions. Local runtimes usually need the model downloaded and reachable before Refact can use it.

## 7. Review Privacy Expectations

Project trajectories, task data, knowledge, provider settings, and usage summaries are stored locally. Network requests go only to configured providers, local endpoints that you point Refact at, and integrations you enable.

Next: read [Configure Providers](/byok/) and [Supported Models](/supported-models/) for provider-specific setup details.
