# Third-party asset notices

The files in `assets/static/vendor/` are vendored only for the NFL-data admin
shell.

- HTMX (`vendor/js/htmx.min.js`): BSD 2-Clause License. Upstream: `https://github.com/bigskysoftware/htmx`.
- Geist and Geist Mono (`vendor/fonts/*.woff2`): SIL Open Font License 1.1. Upstream: `https://github.com/vercel/geist-font`.
- DaisyUI plugin sources in `styles/daisyui*.mjs`: MIT License. Upstream: `https://github.com/saadeghi/daisyui`.

The DaisyUI sources are build-time inputs; they are not runtime browser assets.
