import { describe, expect, it } from "vitest";
import { browserName } from "./device";

describe("browserName", () => {
  it("names the browser and the system it runs on", () => {
    const chrome = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/130.0 Safari/537.36";
    expect(browserName(chrome)).toBe("Chrome on Windows");
    const edge = "Mozilla/5.0 (Windows NT 10.0) AppleWebKit/537.36 Chrome/130.0 Safari/537.36 Edg/130.0";
    expect(browserName(edge)).toBe("Edge on Windows");
    const firefox = "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0";
    expect(browserName(firefox)).toBe("Firefox on Linux");
    const phone = "Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 Chrome/130.0 Mobile Safari/537.36";
    expect(browserName(phone)).toBe("Chrome on Android");
  });

  it("says less when it cannot tell", () => {
    expect(browserName("SomeBot/1.0")).toBe("Browser");
    expect(browserName("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) Version/18.0 Safari/605.1.15")).toBe(
      "Safari on Mac",
    );
  });
});
