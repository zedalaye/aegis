import { describe, expect, it } from "vitest";

import { DOLLAR, editableDollars, formatDollars, parseDollars } from "./money";

describe("money", () => {
  it("formats micro-dollars as the runtime does", () => {
    expect(formatDollars(500_000)).toBe("$0.50");
    expect(formatDollars(12 * DOLLAR)).toBe("$12.00");
    expect(formatDollars(1_234)).toBe("$0.001234");
    expect(formatDollars(75_000)).toBe("$0.075");
  });

  it("parses what a person types, exactly", () => {
    expect(parseDollars("")).toEqual({ kind: "blank" });
    expect(parseDollars(" $0.5 ")).toEqual({ kind: "amount", micros: 500_000 });
    expect(parseDollars("3")).toEqual({ kind: "amount", micros: 3 * DOLLAR });
    expect(parseDollars("0.075")).toEqual({ kind: "amount", micros: 75_000 });
    expect(parseDollars("0.1")).toEqual({ kind: "amount", micros: 100_000 });
    expect(parseDollars("1.0000001")).toEqual({ kind: "invalid" });
    expect(parseDollars("-1")).toEqual({ kind: "invalid" });
    expect(parseDollars("ten")).toEqual({ kind: "invalid" });
  });

  it("round-trips an amount through its editable form", () => {
    for (const micros of [0, 75_000, 500_000, 3 * DOLLAR, 1_234]) {
      const parsed = parseDollars(editableDollars(micros));
      expect(parsed).toEqual({ kind: "amount", micros });
    }
    expect(editableDollars(null)).toBe("");
  });
});
