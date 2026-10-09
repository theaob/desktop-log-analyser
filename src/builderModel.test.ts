import { describe, expect, it } from "vitest";
import type { Query, Row } from "./api";
import { fieldSuggestions, isComplete, isNumericLiteral, move, pickValue, regexpGroups, toModel, toQuery, type Op } from "./builderModel";

const query: Query = {
  selector: [{ name: "level", op: "=", value: "error" }],
  stages: [
    { type: "line", op: "|=", values: ["timeout", "refused"] },
    { type: "json" },
    { type: "filter", expr: { type: "cmp", name: "status", op: ">=", value: { type: "number", value: 500 }, text: "" } },
    { type: "filter", expr: { type: "cmp", name: "took", op: ">", value: { type: "duration", value: 250 }, text: "250ms" } },
    {
      type: "filter",
      expr: {
        type: "or",
        left: { type: "cmp", name: "a", op: "==", value: { type: "string", value: "x" }, text: "" },
        right: { type: "cmp", name: "b", op: "==", value: { type: "string", value: "y" }, text: "" },
      },
    },
  ],
};

describe("toModel / toQuery", () => {
  it("round-trips a query the builder can show", () => {
    const back = toQuery(toModel(query));
    expect(back.selector).toEqual(query.selector);
    expect(back.stages[0]).toEqual(query.stages[0]);
    expect(back.stages[1]).toEqual({ type: "json" });
    expect(back.stages[2]).toEqual(query.stages[2]);
    // Durations keep their text, which the formatter writes verbatim.
    expect(back.stages[3]).toMatchObject({ type: "filter", expr: { name: "took", op: ">", text: "250ms" } });
    expect(back.stages[4]).toEqual(query.stages[4]);
  });

  it("leaves out rows that are still empty", () => {
    const m = toModel(query);
    m.labels.push({ id: 0, name: "source", op: "=", value: "" });
    m.ops.push({ id: 0, kind: "line", op: "|=", values: [""] });
    m.ops.push({ id: 0, kind: "field", name: "user", op: "==", value: "", numeric: false });
    m.ops.push({ id: 0, kind: "parser", parser: "regexp", pattern: "" });
    const back = toQuery(m);
    expect(back.selector).toHaveLength(1);
    expect(back.stages).toHaveLength(query.stages.length);
  });

  it("drops empty alternatives from line filters", () => {
    const back = toQuery({ labels: [], ops: [{ id: 1, kind: "line", op: "!=", values: ["", "debug"] }] });
    expect(back.stages).toEqual([{ type: "line", op: "!=", values: ["debug"] }]);
  });

  it("writes string field filters as strings and ordering filters as numbers", () => {
    const back = toQuery({
      labels: [],
      ops: [
        { id: 1, kind: "field", name: "user", op: "==", value: "42", numeric: false },
        { id: 2, kind: "field", name: "bytes", op: "<", value: "10MB", numeric: false },
      ],
    });
    expect(back.stages[0]).toMatchObject({ expr: { value: { type: "string", value: "42" } } });
    expect(back.stages[1]).toMatchObject({ expr: { value: { type: "number" }, text: "10MB" } });
  });
});

describe("isComplete", () => {
  it("rejects ordering filters whose value isn't a number", () => {
    const op: Op = { id: 1, kind: "field", name: "took", op: ">", value: "fast", numeric: false };
    expect(isComplete(op)).toBe(false);
    expect(isComplete({ ...op, value: "1.5s" })).toBe(true);
  });
});

describe("helpers", () => {
  it("recognises numbers, durations and sizes", () => {
    for (const v of ["5", "-1.5", "250ms", "1h30m", "10MB", "512KiB"]) expect(isNumericLiteral(v)).toBe(true);
    for (const v of ["", "abc", "10 MB", "ms"]) expect(isNumericLiteral(v)).toBe(false);
  });

  it("picks values, adding regex alternatives", () => {
    expect(pickValue("=", "info", "warn")).toBe("warn");
    expect(pickValue("=~", "", "a.b")).toBe("a\\.b");
    expect(pickValue("=~", "warn", "error")).toBe("warn|error");
    expect(pickValue("=~", "warn|error", "error")).toBe("warn|error");
  });

  it("moves items", () => {
    expect(move([1, 2, 3], 0, 2)).toEqual([2, 3, 1]);
    expect(move([1, 2, 3], 0, -1)).toEqual([1, 2, 3]);
  });

  it("finds named groups", () => {
    expect(regexpGroups("took (?P<ms>\\d+)ms user=(?<user>\\w+)")).toEqual(["ms", "user"]);
  });

  it("counts field values in rows", () => {
    const row = (parsed: [string, string][]) => ({ parsed, fields: [] }) as unknown as Row;
    const s = fieldSuggestions([row([["status", "500"]]), row([["status", "200"]]), row([["status", "500"]])]);
    expect(s.get("status")).toEqual([
      { value: "500", count: 2 },
      { value: "200", count: 1 },
    ]);
  });
});
