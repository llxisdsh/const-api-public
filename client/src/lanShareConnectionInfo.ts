export type LanShareConnectionInfo = {
  channelName: string;
  baseUrl: string;
  apiKey: string;
};

const LAN_SHARE_HEADINGS = new Set([
  "const api 局域网共享渠道",
  "const api lan sharing channel",
]);

function splitField(line: string): [string, string] | null {
  const asciiColon = line.indexOf(":");
  const fullWidthColon = line.indexOf("：");
  const separator = asciiColon < 0
    ? fullWidthColon
    : fullWidthColon < 0 ? asciiColon : Math.min(asciiColon, fullWidthColon);
  if (separator < 0) return null;
  return [line.slice(0, separator).trim(), line.slice(separator + 1).trim()];
}

function normalizedFieldName(value: string) {
  return value.toLocaleLowerCase().replace(/\s+/g, "");
}

function validLanShareBaseUrl(value: string) {
  try {
    const url = new URL(value);
    return (url.protocol === "http:" || url.protocol === "https:") && Boolean(url.host);
  } catch {
    return false;
  }
}

export function lanShareOpenAiBaseUrl(connectUrl: string) {
  const value = connectUrl.trim();
  if (!value) return "";
  try {
    const url = new URL(value);
    const pathname = url.pathname.replace(/\/+$/, "");
    url.pathname = /\/v1$/i.test(pathname) ? pathname : `${pathname}/v1`;
    url.search = "";
    url.hash = "";
    return url.toString().replace(/\/$/, "");
  } catch {
    return `${value.replace(/\/+$/, "")}/v1`;
  }
}

export function lanShareConnectionHost(baseUrl: string) {
  try {
    return new URL(baseUrl).hostname;
  } catch {
    return "";
  }
}

export function parseLanShareConnectionInfo(text: string): LanShareConnectionInfo | null {
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  if (!lines.some((line) => LAN_SHARE_HEADINGS.has(line.toLocaleLowerCase()))) return null;

  let channelName = "";
  let baseUrl = "";
  let apiKey = "";
  for (const line of lines) {
    const field = splitField(line);
    if (!field) continue;
    const [rawName, value] = field;
    switch (normalizedFieldName(rawName)) {
      case "渠道名称":
      case "channelname":
        channelName = value;
        break;
      case "baseurl":
        baseUrl = value;
        break;
      case "apikey":
        apiKey = value;
        break;
    }
  }

  if (!channelName || !apiKey || !validLanShareBaseUrl(baseUrl)) return null;
  return { channelName, baseUrl, apiKey };
}
