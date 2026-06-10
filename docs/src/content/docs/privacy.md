---
title: Privacy
description: How data moves and where it is stored in local-first Refact.
---

Refact runs locally and uses the providers, local endpoints, and integrations that you configure. There is no hosted Refact account requirement in the normal setup path.

## What Stays Local

Refact stores operational data in local user and project directories, including:

- Provider settings and credentials.
- Chat trajectories and task metadata.
- Project knowledge and local indexes.
- Usage summaries and model-selection settings.
- Integration configuration files.

Typical locations include user Refact config/cache directories and project `.refact/` directories.

## What Can Leave Your Machine

Network requests are created only for providers, endpoints, and integrations that you configure or enable. Examples include:

- Chat, agent, completion, or embedding requests to a hosted provider.
- Model catalog refreshes from providers that support discovery.
- Calls to GitHub, GitLab, Docker, database, MCP, browser, shell, or command-service integrations when you enable those tools.
- Requests to local runtimes such as Ollama, LM Studio, or vLLM. These stay local only when the endpoint you configure is local.

Prompts can include code context, selected files, tool results, and instructions needed for the workflow. Review each provider and integration policy before sending sensitive data.

## What Refact Does Not Require

- A hosted Refact login.
- A Refact-issued model API key.
- A Refact-operated model relay.
- A hosted team workspace.
- A product credit balance for model calls.

## Controlling Data Flow

You control which providers are configured, which integrations are enabled, and which files or selections are attached to a chat. Disable providers or integrations you do not want Refact to contact.

For local-only workflows, use local runtimes and avoid enabling external integrations.
