import starlight from "@astrojs/starlight";
import { defineConfig } from "astro/config";
import mermaid from "astro-mermaid";
import starlightLinksValidator from "starlight-links-validator";

export default defineConfig({
  site: "https://tamis.larve.net",
  base: "/docs",
  outDir: "../site/docs",
  integrations: [
    mermaid(),
    starlight({
      title: "tamis",
      favicon: "/favicon.svg",
      customCss: ["./src/styles/theme.css"],
      description: "A spam filter and Sieve mail sorter for remote mailboxes.",
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/hugoh/tamis",
        },
      ],
      editLink: { baseUrl: "https://github.com/hugoh/tamis/edit/main/docs/" },
      plugins: [starlightLinksValidator({ errorOnRelativeLinks: false })],
      sidebar: [
        { label: "← tamis home", link: "https://tamis.larve.net/" },
        "install",
        "quick-start",
        {
          label: "User guide",
          items: [
            "user-guide/filing-rules",
            "user-guide/web-interface",
            "user-guide/passwords",
            "user-guide/running-as-a-service",
            "user-guide/monitoring",
            "user-guide/probe",
          ],
        },
        {
          label: "Reference",
          items: [
            "reference/where-tamis-keeps-things",
            "reference/sieve-extensions",
          ],
        },
        {
          label: "How it works",
          items: [
            "how-it-works/engine",
            "how-it-works/rules",
            "how-it-works/imap",
            "how-it-works/evaluation",
            "how-it-works/design",
          ],
        },
        "contributing",
      ],
    }),
  ],
});
