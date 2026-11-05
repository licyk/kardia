import { describe, expect, it } from "vitest";

import { HOME_SECTION_IDS, HOME_SECTIONS, nextHomeSection } from "@/lib/home-sections";
import { baseOptions } from "@/lib/layout.shared";

describe("home section manifest", () => {
  it("keeps section ids unique", () => {
    expect(new Set(HOME_SECTION_IDS).size).toBe(HOME_SECTION_IDS.length);
  });

  it("starts at the hero", () => {
    expect(HOME_SECTION_IDS[0]).toBe("hero");
  });

  it("gives every section a rail label", () => {
    for (const section of HOME_SECTIONS) {
      expect(section.railLabel.length).toBeGreaterThan(0);
    }
  });
});

describe("nextHomeSection", () => {
  it("advances exactly one section", () => {
    expect(nextHomeSection("hero")?.id).toBe(HOME_SECTION_IDS[1]);
    expect(nextHomeSection(HOME_SECTION_IDS[2])?.id).toBe(HOME_SECTION_IDS[3]);
  });

  it("does not skip ahead to the highlights section", () => {
    // The hero button stops at whatever section immediately follows the hero.
    expect(nextHomeSection("hero")?.id).toBe("features");
  });

  it("returns undefined past the last section and for unknown ids", () => {
    expect(nextHomeSection(HOME_SECTION_IDS.at(-1) as string)).toBeUndefined();
    expect(nextHomeSection("not-a-section")).toBeUndefined();
  });
});

describe("header navigation", () => {
  const links = baseOptions().links ?? [];

  it("carries no in-page section jumps", () => {
    const anchors = links.filter(
      (link) => "url" in link && typeof link.url === "string" && link.url.startsWith("/#"),
    );

    expect(anchors).toEqual([]);
  });

  it("carries no custom action button", () => {
    expect(links.filter((link) => link.type === "custom")).toEqual([]);
  });

  it("links the documentation and the repository", () => {
    expect(links).toEqual([{ text: "文档", url: "/docs", active: "nested-url" }]);
    expect(baseOptions().githubUrl).toBe("https://github.com/licyk/kardia");
  });
});
