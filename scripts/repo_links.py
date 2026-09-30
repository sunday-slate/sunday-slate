"""MkDocs hook: link repo files as examples from docs pages.

Registered via `hooks:` in .config/mkdocs.yml (mkdocs >= 1.2 lightweight plugin API).

Rewrites markdown link targets that point at real files in the repo but
*outside* docs_dir (e.g. `[x](../.github/workflows/docs.yml)`) into GitHub
blob URLs, so they are valid on the published site.

Rules:
- A relative target is resolved against the page directory. If that resolves
  inside docs/ to an existing page/asset, mkdocs handles it — repo rewriting
  never hijacks a working docs link.
- If the docs-side resolution is missing, successive `../` steps are tried
  (still resolving against the page directory) until the target lands outside
  docs/; if it then exists among repo files, the link is rewritten to GitHub.
- Targets that never resolve — inside or outside the repo — are left alone,
  so mkdocs' own link validation still fails strict builds on typos.
- Anchors (`#frag`) are preserved; `http(s)://`, `mailto:`, `/`-rooted and
  image (`![..](..)`) targets are untouched.

Keeping relative targets in the source markdown also means the links still
work when the file is viewed on github.com.
"""

import os
import re

# Fallbacks used only when the mkdocs config has no repo_url/edit_uri.
REPO_URL = "https://github.com/sunday-slate/sunday-slate"
BRANCH = "main"

# [text](target "optional title"), not preceded by `!` (images stay untouched)
# and not an already-absolute URL/anchor target.
LINK_PATTERN = r'(?<!!)\[(?P<text>[^\]]*)\]\((?P<target>[^)\s]+)(?:\s+"[^"]*")?\)'

_LINK_RE = re.compile(LINK_PATTERN)
_SCHEMES = ("http://", "https://", "mailto:")
_MAX_PEEL = 16


def _repo_root(config):
    """Repo root — the parent of the directory holding the docs source tree.

    Deriving it from docs_dir keeps the hook independent of where the mkdocs
    config file lives (e.g. repo root or .config/).
    """
    return os.path.dirname(_docs_dir_abs(config))


def _docs_dir_abs(config):
    docs_dir = config["docs_dir"]
    if not os.path.isabs(docs_dir):
        docs_dir = os.path.join(os.path.dirname(os.path.abspath(config["config_file_path"])), docs_dir)
    return os.path.normpath(docs_dir)


def _inside_docs(path, docs_dir):
    rel = os.path.relpath(path, docs_dir)
    return not os.path.isabs(rel) and (rel == "." or not rel.startswith(".." + os.sep))


def _github_defaults(config):
    """(repo_url, branch) — repo_url/branch come from the mkdocs config when set."""
    repo_url = (config.get("repo_url") or REPO_URL).rstrip("/")
    branch = None
    edit_uri = (config.get("edit_uri") or "").strip("/")
    parts = edit_uri.split("/") if edit_uri else []
    if len(parts) >= 2 and parts[0] in ("edit", "blob", "tree"):
        branch = parts[1]
    return repo_url, branch or BRANCH


def _rewrite_target(target, page_dir, config, repo_url, branch):
    """GitHub blob URL for targets that denominate repo files outside docs/, else None."""
    if target.startswith(("#", "/") + _SCHEMES):
        return None

    base, _, fragment = target.partition("#")
    if not base:
        return None

    docs_dir = _docs_dir_abs(config)

    candidate = None
    for depth in range(_MAX_PEEL + 1):
        steps = "" if depth == 0 else (".." + os.sep) * depth
        trial = os.path.normpath(os.path.join(page_dir, steps, base))
        if _inside_docs(trial, docs_dir):
            if os.path.exists(trial):
                return None  # resolves to a real docs page/asset: mkdocs owns it
            continue  # missing inside docs/: peel one more ../ step
        candidate = trial
        break

    if candidate is None:
        return None

    repo_root = _repo_root(config)
    repo_rel = os.path.relpath(candidate, repo_root)
    if repo_rel.startswith(".." + os.sep) or os.path.isabs(repo_rel):
        return None  # outside the repo root; not linkable
    if not os.path.isfile(os.path.join(repo_root, repo_rel)):
        return None  # unresolved: mkdocs validation should flag it

    repo_url, branch = _github_defaults(config)
    url = f"{repo_url}/blob/{branch}/{repo_rel.replace(os.sep, '/')}"
    return f"{url}#{fragment}" if fragment else url


def on_page_markdown(markdown, page, config, files):
    src = getattr(page.file, "abs_src_path", None)
    if not src:
        src = os.path.join(_docs_dir_abs(config), page.file.src_path)
    page_dir = os.path.dirname(src)

    repo_url, branch = _github_defaults(config)

    def swap(match):
        new_target = _rewrite_target(match.group("target"), page_dir, config, repo_url, branch)
        if new_target is None:
            return match.group(0)
        return match.group(0).replace(match.group("target"), new_target, 1)

    return _LINK_RE.sub(swap, markdown)
