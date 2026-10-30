# Public website

This is an independent Astro project with its own `package.json` and
`pnpm-lock.yaml`. From the repository root, install with
`pnpm --dir site install --frozen-lockfile` and verify with `pnpm --dir site build`.
Root desktop UI tests do not cover the site. Generated output is `site/dist/`.

Shared product/version/download links live in `src/site.ts`; URL/base and sitemap
configuration live in `astro.config.mjs`. Update these together when changing the
fork's published destination. Keep attribution links intentional.

Pages use `src/layouts/Base.astro`; screenshots and public assets are in `public/`.
Check links under the configured `/hark` base path. Publication is controlled by
`.github/workflows/pages.yml`; deployment requires repository variable
`HARK_ENABLE_PAGES=true`. A successful local build does not publish it.
