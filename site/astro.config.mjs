import { defineConfig } from "astro/config";

// yamete.app is an apex domain, so `base` stays "/" and public/CNAME carries the hostname.
// Apex domains cannot use a CNAME record — see site/README.md for the four A records.
export default defineConfig({
  site: "https://yamete.app",
  trailingSlash: "never",
  build: { inlineStylesheets: "always" },
});
