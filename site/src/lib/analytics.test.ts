import { describe, expect, it } from "vitest";

import { UMAMI_SCRIPT } from "@/lib/analytics";

describe("analytics", () => {
  it("reports to kardia's Umami website", () => {
    expect(UMAMI_SCRIPT["data-website-id"]).toBe("0fbf20c3-e35e-4017-a3de-62b51a57cb55");
    expect(UMAMI_SCRIPT.src).toBe("https://licyk-umami.netlify.app/script.js");
  });
});
