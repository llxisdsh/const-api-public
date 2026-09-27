// The runtime validates and normalizes this shared server policy before IPC.
export type ModelPresentationPolicy = {
  schema_version: number;
  rules: readonly { pattern: string; hidden: boolean }[];
};

// Keep the first source for each short name, but retain its complete request ID.
export function modelDisplayName(model: string): string {
  const value = model.trim().replace(/\[1m\]$/i, "").trim();
  return (value.slice(value.lastIndexOf("/") + 1) || value).toLowerCase();
}

export function modelContextLabel(tokens: number | undefined): string {
  if (!tokens || !Number.isSafeInteger(tokens) || tokens <= 0) return "";
  if (tokens >= 1_000_000) return `${Math.floor(tokens / 10_000) / 100}M`;
  if (tokens >= 1_000) return `${Math.floor(tokens / 100) / 10}K`;
  return String(tokens);
}

export function modelDisplayNames(
  models: readonly string[],
  presentation?: ModelPresentationPolicy | null,
): Map<string, string> {
  const labels = new Map<string, string>();
  const seen = new Set<string>();
  for (const model of models) {
    const name = modelDisplayName(model);
    const key = name.toLowerCase();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    labels.set(model, name);
  }
  const rules = presentation?.rules ?? [];
  const ranked = [...labels].map(([id, label]) => {
    const index = rules.findIndex((rule) => rule.pattern === label
      || (rule.pattern.endsWith("*") && label.startsWith(rule.pattern.slice(0, -1))));
    return { id, label, priority: index < 0 ? 0 : rules.length - index };
  });
  // Display order only: do not apply tool visibility rules to a permissions editor.
  ranked.sort((left, right) => right.priority - left.priority
    || (left.label < right.label ? -1 : left.label > right.label ? 1 : 0));
  return new Map(ranked.map(({ id, label }) => [id, label]));
}

export function modelInputOptions(models: readonly string[], selected = "") {
  const selectedName = modelDisplayName(selected).toLowerCase();
  const values = selected && !models.some((model) => modelDisplayName(model).toLowerCase() === selectedName)
    ? [selected, ...models]
    : models;
  return [...modelDisplayNames(values)].map(([value, label]) => ({ value, label }));
}
