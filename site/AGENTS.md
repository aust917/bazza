# Public website

This is an independent Astro project with its own `package.json` and
`pnpm-lock.yaml`. From the repository root, install with
`pnpm --dir site install --frozen-lockfile` and verify with `pnpm --dir site build`.
Root desktop UI tests do not cover the site. Generated output is `site/dist/`.

Shared product/version and attribution links live in `src/site.ts`; URL/base
configuration lives in `astro.config.mjs`. The local site uses localhost, noindex
and no sitemap; it offers no public Bazza installer. Update these together only
when publication is authorized. Keep upstream attribution links intentional.

Pages use `src/layouts/Base.astro`; screenshots and public assets are in `public/`.
Check links under the configured `/bazza` base path. Publication is controlled by
`.github/workflows/pages.yml`; deployment requires repository variable
`BAZZA_ENABLE_PAGES=true`. A successful local build does not publish it.
