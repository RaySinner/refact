---
title: FAQ
description: Frequently asked questions about local-first Refact.
---

## Do I need a Refact account?

No. Refact runs locally in your IDE and uses the providers or local runtimes you configure.

## Do I need to run a separate backend?

No. The VS Code extension and JetBrains plugin start the local `refact-lsp` engine. Configure model providers or local runtimes in the Refact UI.

## Which providers can I use?

Refact supports Anthropic, OpenAI, OpenAI Responses, OpenAI Codex, OpenRouter, Ollama, LM Studio, vLLM, Groq, DeepSeek, Doubao, xAI, Gemini, Qwen, Kimi, Zhipu, MiniMax, GitHub Copilot, Custom, and Claude Code provider flows.

## Can I use local models?

Yes. Use Ollama, LM Studio, vLLM, or a custom OpenAI-compatible endpoint. Start the runtime, make the model available there, add the runtime in Refact, and select the model in **Default Models**.

## How am I billed?

Refact does not sell model access. Billing, quotas, rate limits, and model access are handled by the provider or local runtime you configure.

## Does Refact send my code to Smallcloud or Refact-hosted services?

The normal setup path sends requests only to configured providers, endpoints, and integrations. Local project data such as trajectories, tasks, knowledge, provider settings, and usage summaries are stored locally.

## Does local-first mean completely offline?

The IDE UI and Refact engine run locally. A workflow is fully local only when the configured model runtime and enabled tools are local. Hosted model providers and external integrations require network access.

## Where are model capabilities stored?

The engine includes local capability metadata and combines it with configured provider data. This is why the model documentation describes provider families and capabilities instead of a fixed exhaustive model list.

## What default models should I choose?

Start with one strong chat or agent model, one faster chat model if available, and one completion-capable model if you want inline suggestions. Add an embedding model when you use semantic search or knowledge workflows.

## Can I change providers later?

Yes. Add, remove, or disable providers in **Provider Setup**, then update **Default Models**. Existing local trajectories and task data remain in your project/user Refact directories.
