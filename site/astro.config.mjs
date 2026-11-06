import { defineConfig } from "astro/config";

export default defineConfig({
  site: "http://localhost:4321",
  base: "/bazza",
  trailingSlash: "always",
  build: { format: "directory" },
});
