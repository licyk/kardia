import { describe, expect, it } from "vitest";

import { GISCUS_ATTRIBUTES, giscusLanguage, giscusTerm, giscusTheme } from "@/lib/giscus";
import { LOCALES } from "@/lib/i18n";

describe("giscus", () => {
  it("uses kardia's discussions, mapped by the page's own term", () => {
    expect(GISCUS_ATTRIBUTES["data-repo"]).toBe("licyk/kardia");
    expect(GISCUS_ATTRIBUTES["data-repo-id"]).toBe("R_kgDOVjB9NA");
    expect(GISCUS_ATTRIBUTES["data-category"]).toBe("Comment");
    expect(GISCUS_ATTRIBUTES["data-category-id"]).toBe("DIC_kwDOVjB9NM4DKbXu");
    expect(GISCUS_ATTRIBUTES["data-mapping"]).toBe("specific");
    expect(GISCUS_ATTRIBUTES["data-strict"]).toBe("1");
  });

  it("maps one discussion to each page, with or without a trailing slash", () => {
    expect(giscusTerm("/docs/quick-start", "/")).toBe("docs/quick-start");
    expect(giscusTerm("/docs/quick-start/", "/")).toBe("docs/quick-start");
    expect(giscusTerm("/docs/", "/")).toBe("docs");
    expect(giscusTerm("/", "/")).toBe("index");
  });

  it("gives a page the same discussion on GitHub Pages (/kardia/) and at a root", () => {
    expect(giscusTerm("/kardia/docs/guide-live", "/kardia/")).toBe("docs/guide-live");
    expect(giscusTerm("/kardia/docs/guide-live/", "/kardia")).toBe("docs/guide-live");
    expect(giscusTerm("/kardia/", "/kardia/")).toBe("index");
    expect(giscusTerm("/kardiax/docs", "/kardia/")).toBe("kardiax/docs");
  });

  it("gives both languages of a page the same discussion", () => {
    expect(giscusTerm("/en/docs/guide-live", "/")).toBe("docs/guide-live");
    expect(giscusTerm("/kardia/zh-CN/docs/guide-live", "/kardia/")).toBe("docs/guide-live");
  });

  it("follows the site's resolved theme", () => {
    expect(giscusTheme("dark")).toBe("dark");
    expect(giscusTheme("light")).toBe("light");
    expect(giscusTheme(undefined)).toBe("light");
  });

  it("has a giscus language for every site locale", () => {
    for (const locale of LOCALES) expect(giscusLanguage(locale)).toBeTruthy();
    expect(giscusLanguage("zh-CN")).toBe("zh-CN");
    expect(giscusLanguage("en")).toBe("en");
  });
});
