These documents are being built up over time as notes and references to how Sunday Slate is developed. This will include patterns, code organization, notes on various library usage, etc. Eventually, more standard user and dev guides that pull from these documents will be published. In the meantime, any topic here as it pertains to the project should be added here and it'll get re-organized.

## Authoring and previewing

Docs are plain markdown in `docs/`; any text editor works. From the repo root:

- `just docs` serves the site at <http://localhost:8000> with live reload
  while you edit.
- `just docs-build` does a one-shot build; CI uses the same build with
  `--strict`, so navigation and file problems mkdocs reports as warnings
  fail the build there too. Wiki-style `[[links]]` are invisible to
  mkdocs; a grep pass over `docs/` catches those.

## Publishing

Pushes to `main` that touch the docs build the site with
[MkDocs](https://www.mkdocs.org/) (Material theme) and publish it to GitHub
Pages via
[.github/workflows/docs.yml](../.github/workflows/docs.yml).

## mkDocs

Using mkDocs which has a planned 2.0. See note on [mkDocs 2.0 upgrade](./mkdocs-upgrade.md)