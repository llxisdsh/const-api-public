import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const localeFiles = {
  "en-US": new URL("../src/i18n/locales/en-US.json", import.meta.url),
  "zh-CN": new URL("../src/i18n/locales/zh-CN.json", import.meta.url),
};
const sourceDriverFile = new URL("../source-drivers.json", import.meta.url);
const sourceRoot = new URL("../src/", import.meta.url);

function flatten(value, prefix = "", output = new Map()) {
  if (typeof value === "string") {
    output.set(prefix, value);
    return output;
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`Translation value at "${prefix || "<root>"}" must be an object or string`);
  }
  for (const [key, child] of Object.entries(value)) {
    flatten(child, prefix ? `${prefix}.${key}` : key, output);
  }
  return output;
}

const locales = new Map();
for (const [language, file] of Object.entries(localeFiles)) {
  const path = fileURLToPath(file);
  const parsed = JSON.parse(await readFile(file, "utf8"));
  const entries = flatten(parsed);
  const empty = [...entries].filter(([, value]) => value.trim().length === 0);
  if (empty.length > 0) {
    throw new Error(`${language} has empty translations:\n${empty.map(([key]) => `  - ${key}`).join("\n")}`);
  }
  locales.set(language, entries);
}

const englishKeys = new Set(locales.get("en-US").keys());
const chineseKeys = new Set(locales.get("zh-CN").keys());
const missingChinese = [...englishKeys].filter((key) => !chineseKeys.has(key));
const missingEnglish = [...chineseKeys].filter((key) => !englishKeys.has(key));

if (missingChinese.length > 0 || missingEnglish.length > 0) {
  const sections = [];
  if (missingChinese.length > 0) {
    sections.push(`Missing in zh-CN:\n${missingChinese.map((key) => `  - ${key}`).join("\n")}`);
  }
  if (missingEnglish.length > 0) {
    sections.push(`Missing in en-US:\n${missingEnglish.map((key) => `  - ${key}`).join("\n")}`);
  }
  throw new Error(sections.join("\n\n"));
}

const sourceDriverManifest = JSON.parse(await readFile(fileURLToPath(sourceDriverFile), "utf8"));
const missingSourceDriverKeys = [];
for (const driver of sourceDriverManifest.drivers ?? []) {
  for (const language of Object.keys(localeFiles)) {
    const entries = locales.get(language);
    for (const field of ["title", "description"]) {
      const key = `sourceDrivers.${driver.id}.${field}`;
      if (!entries?.has(key)) missingSourceDriverKeys.push(`${language}: ${key}`);
    }
  }
}
if (missingSourceDriverKeys.length > 0) {
  throw new Error(
    `Source-driver translations are incomplete:\n${missingSourceDriverKeys.map((key) => `  - ${key}`).join("\n")}`,
  );
}

async function sourceFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const absolute = new URL(entry.name + (entry.isDirectory() ? "/" : ""), directory);
    if (entry.isDirectory()) {
      files.push(...await sourceFiles(absolute));
    } else if (/\.(?:ts|tsx)$/.test(entry.name)) {
      files.push(absolute);
    }
  }
  return files;
}

const referencedKeys = new Set();
for (const file of await sourceFiles(sourceRoot)) {
  const source = await readFile(fileURLToPath(file), "utf8");
  for (const match of source.matchAll(/\b(?:t|tr)\(\s*["'`]([^"'`$]+)["'`]/g)) {
    referencedKeys.add(match[1]);
  }
}

const missingReferencedKeys = [];
for (const key of [...referencedKeys].sort()) {
  for (const language of Object.keys(localeFiles)) {
    if (!locales.get(language)?.has(key)) {
      missingReferencedKeys.push(`${language}: ${key}`);
    }
  }
}
if (missingReferencedKeys.length > 0) {
  throw new Error(
    `Translations referenced by the renderer are missing:\n${missingReferencedKeys.map((key) => `  - ${key}`).join("\n")}`,
  );
}

console.log(
  `i18n resources are aligned (${englishKeys.size} keys per language; ${referencedKeys.size} referenced keys; ${sourceDriverManifest.drivers.length} source drivers covered).`,
);
