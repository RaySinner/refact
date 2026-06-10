import { describe, it, expect } from "vitest";
import {
  extractParamsFromSchema,
  toInputSchema,
  fromInputSchema,
} from "../utils/toolSchema";

describe("extractParamsFromSchema", () => {
  it("extracts params from a valid schema", () => {
    const schema = {
      type: "object",
      properties: {
        symbol: { type: "string", description: "A symbol name" },
        count: { type: "integer", description: "Count value" },
      },
      required: ["symbol"],
    };
    const params = extractParamsFromSchema(schema);
    expect(params).toHaveLength(2);
    expect(params[0]).toEqual({
      name: "symbol",
      type: "string",
      description: "A symbol name",
    });
    expect(params[1]).toEqual({
      name: "count",
      type: "integer",
      description: "Count value",
    });
  });

  it("returns empty array for schema with no properties", () => {
    const schema = { type: "object" };
    expect(extractParamsFromSchema(schema)).toEqual([]);
  });

  it("returns empty array for empty schema", () => {
    expect(extractParamsFromSchema({})).toEqual([]);
  });

  it("defaults type to string when missing", () => {
    const schema = {
      type: "object",
      properties: {
        path: { description: "File path" },
      },
    };
    const params = extractParamsFromSchema(schema);
    expect(params[0].type).toBe("string");
  });

  it("defaults description to empty string when missing", () => {
    const schema = {
      type: "object",
      properties: {
        flag: { type: "boolean" },
      },
    };
    const params = extractParamsFromSchema(schema);
    expect(params[0].description).toBe("");
  });
});

describe("toInputSchema", () => {
  it("produces valid JSON Schema from params", () => {
    const params = [
      { name: "query", type: "string", description: "Search query" },
      { name: "limit", type: "integer", description: "Max results" },
    ];
    const schema = toInputSchema(params, ["query"]);
    expect(schema).toEqual({
      type: "object",
      properties: {
        query: { type: "string", description: "Search query" },
        limit: { type: "integer", description: "Max results" },
      },
      required: ["query"],
    });
  });

  it("handles empty params", () => {
    const schema = toInputSchema([], []);
    expect(schema).toEqual({ type: "object", properties: {}, required: [] });
  });
});

describe("fromInputSchema round-trip", () => {
  it("round-trips params and required through toInputSchema/fromInputSchema", () => {
    const originalParams = [
      { name: "path", type: "string", description: "File path" },
      { name: "content", type: "string", description: "File content" },
    ];
    const originalRequired = ["path"];

    const schema = toInputSchema(originalParams, originalRequired);
    const { params, required } = fromInputSchema(schema);

    expect(params).toEqual(originalParams);
    expect(required).toEqual(originalRequired);
  });

  it("handles schema without required field", () => {
    const schema = {
      type: "object",
      properties: {
        name: { type: "string", description: "Name" },
      },
    };
    const { params, required } = fromInputSchema(schema);
    expect(params).toHaveLength(1);
    expect(required).toEqual([]);
  });
});
