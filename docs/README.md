# Refact docs site

This folder contains the public Refact documentation site built with Astro and Starlight. The site explains how to install Refact, configure BYOK/local providers, use IDE chat and agent workflows, and develop against the current monorepo.

## Project layout

| Path | Purpose |
| --- | --- |
| `src/content/docs/` | Markdown documentation pages routed by Astro content collections |
| `src/assets/` | Images and SVG assets used by docs pages and the theme |
| `src/components/` | Starlight/Astro component overrides such as search and head metadata |
| `src/styles/` | Custom CSS loaded by `astro.config.mjs` |
| `astro.config.mjs` | Site metadata, sidebar navigation, social links, and edit-link configuration |
| `public/` | Static assets copied directly into the built site |

## Install

```bash
cd docs
npm install
```

Use `npm ci` instead when you want a lockfile-reproducible install in CI.

## Commands

These commands come from `docs/package.json`:

| Command | Action |
| --- | --- |
| `npm run dev` | Start the local Astro development server |
| `npm run start` | Alias for `npm run dev` |
| `npm run build` | Build the production site into `dist/` |
| `npm run preview` | Preview the built site locally |
| `npm run astro -- --help` | Run Astro CLI commands with telemetry disabled |

## Authoring notes

- Add or update pages under `src/content/docs/` with frontmatter that includes at least `title` and `description`.
- Keep sidebar entries in `astro.config.mjs` in sync with new or renamed pages.
- Prefer relative links between docs pages and keep external links stable.
- Store reusable images in `src/assets/`; use `public/` only for files that must be served at a fixed URL.
- Run `npm run build` before submitting docs changes.

## Useful links

- [Refact documentation](https://docs.refact.ai/)
- [Astro docs](https://docs.astro.build/)
- [Starlight docs](https://starlight.astro.build/)
