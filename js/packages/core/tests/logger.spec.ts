import { afterEach, describe, expect, it, vi } from "vitest";

import { Context, consoleExporter, hyphenate, Level, type Message } from "#/index";

describe("Logger", () => {
  afterEach(() => void vi.restoreAllMocks());

  it("is named after the plugin, hyphenated", async () => {
    const root = new Context();
    const names: string[] = [];
    await root.plugin(function fooBar(ctx: Context) {
      names.push(ctx.logger().name);
    }).settled();
    expect(names).toEqual(["foo-bar"]);
    expect(root.logger().name).toBe("root");
  });

  it("exports up to the exporter's level, or the logger's (default: info)", () => {
    const root = new Context();
    const all: Message[] = [];
    const quiet: Message[] = [];
    root.addExporter({ export: (m) => all.push(m), level: () => Level.debug });
    root.addExporter({ export: (m) => quiet.push(m) });
    const logger = root.logger("test");
    logger.debug("d");
    logger.info("i");
    logger.withLevel(Level.debug).debug("d2");
    expect(all.map((m) => m.text)).toEqual(["d", "i", "d2"]);
    expect(quiet.map((m) => m.text)).toEqual(["i", "d2"]);
  });

  it("formats arguments, errors and aggregate errors", () => {
    const root = new Context();
    const texts: string[] = [];
    root.addExporter({ export: (m) => texts.push(m.text) });
    const logger = root.logger("test");
    logger.info("a", 1, { b: 2 });
    logger.logError(new AggregateError([new Error("x"), new Error("y")]));
    expect(texts[0]).toBe('a 1 {"b":2}');
    expect(texts.slice(1).map((text) => text.split("\n")[0])).toEqual(["Error: x", "Error: y"]);
  });

  it("keeps a bounded buffer", () => {
    const root = new Context();
    root.setLogBufferSize(2);
    const logger = root.logger("test");
    logger.info("1");
    logger.info("2");
    logger.info("3");
    expect(root.logBuffer().map((m) => m.text)).toEqual(["2", "3"]);
  });

  it("removes an exporter with its effect", async () => {
    const root = new Context();
    const texts: string[] = [];
    const fiber = root.plugin((ctx: Context) => void ctx.addExporter({ export: (m) => texts.push(m.text) }));
    await fiber.settled();
    root.logger("test").info("before");
    await fiber.dispose();
    root.logger("test").info("after");
    expect(texts).toEqual(["before"]);
  });

  it("writes to the console", () => {
    const root = new Context();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
    root.addExporter(consoleExporter({ levels: { default: Level.info } }));
    root.logger("test").warn("careful");
    root.logger("test").debug("hidden");
    expect(warn).toHaveBeenCalledWith("[test] careful");
    expect(debug).not.toHaveBeenCalled();
  });

  it("hyphenates", () => {
    expect(hyphenate("fooBar")).toBe("foo-bar");
    expect(hyphenate("Qux")).toBe("qux");
    expect(hyphenate("foo:driver")).toBe("foo:driver");
    expect(hyphenate("snake_case")).toBe("snake-case");
  });
});
