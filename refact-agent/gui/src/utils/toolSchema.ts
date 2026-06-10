export type ExtractedParam = {
  name: string;
  type: string;
  description: string;
};

export function extractParamsFromSchema(
  schema: Record<string, unknown>,
): ExtractedParam[] {
  const properties = (schema.properties ?? {}) as Record<
    string,
    { type?: string; description?: string }
  >;
  return Object.entries(properties).map(([name, prop]) => ({
    name,
    type: prop.type ?? "string",
    description: prop.description ?? "",
  }));
}

export function toInputSchema(
  params: { name: string; type: string; description: string }[],
  required: string[],
): Record<string, unknown> {
  const properties: Record<string, { type: string; description: string }> = {};
  for (const param of params) {
    properties[param.name] = {
      type: param.type,
      description: param.description,
    };
  }
  return {
    type: "object",
    properties,
    required,
  };
}

export function fromInputSchema(schema: Record<string, unknown>): {
  params: { name: string; type: string; description: string }[];
  required: string[];
} {
  const params = extractParamsFromSchema(schema);
  const required = Array.isArray(schema.required)
    ? (schema.required as string[])
    : [];
  return { params, required };
}
