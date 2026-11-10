function tryDecode(str: string): string {
  try {
    return decodeURIComponent(str);
  } catch {
    return str;
  }
}

function extractBracedParams(pattern: string): {
  transformed: string;
  custom: { ph: string; name: string; regex: string }[];
} {
  let result = "";
  let i = 0;
  const custom: { ph: string; name: string; regex: string }[] = [];
  while (i < pattern.length) {
    if (pattern[i] === ":" && /[a-zA-Z0-9_]/.test(pattern[i + 1] || "")) {
      const nameStart = i + 1;
      let nameEnd = nameStart;
      while (nameEnd < pattern.length && /[a-zA-Z0-9_]/.test(pattern[nameEnd]!)) {
        nameEnd++;
      }
      const name = pattern.slice(nameStart, nameEnd);
      if (pattern[nameEnd] === "{") {
        let depth = 1;
        const regexStart = nameEnd + 1;
        let j = regexStart;
        while (j < pattern.length && depth > 0) {
          if (pattern[j] === "{") depth++;
          else if (pattern[j] === "}") depth--;
          j++;
        }
        if (depth === 0) {
          const regex = pattern.slice(regexStart, j - 1);
          const ph = `__PARAM_CUSTOM_${custom.length}__`;
          custom.push({ ph, name, regex });
          result += ph;
          i = j;
          continue;
        }
      }
    }
    result += pattern[i];
    i++;
  }
  return { transformed: result, custom };
}

function escapeRegex(str: string): string {
  return str.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function compilePattern(
  pattern: string,
  strict = true,
): (pathname: string) => Record<string, string> | undefined {
  if (pattern === "*") {
    return () => ({});
  }

  const { transformed, custom } = extractBracedParams(pattern);

  const optionalParams: { ph: string; name: string }[] = [];
  let transformedPattern = transformed.replace(/\/:([a-zA-Z0-9_]+)\?/g, (_match, name) => {
    const ph = `/__PARAM_OPTIONAL_${optionalParams.length}__`;
    optionalParams.push({ ph, name });
    return ph;
  });

  const isTrailingWildcard = transformedPattern.endsWith("/*");
  if (isTrailingWildcard) {
    transformedPattern = transformedPattern.slice(0, -2);
  }

  const parts = transformedPattern.split("/").filter(Boolean);
  let regexStr = "^";
  const paramNames: string[] = [];

  if (parts.length === 0) {
    regexStr += "\\/";
  } else {
    for (let i = 0; i < parts.length; i++) {
      const part = parts[i]!;
      const opt = optionalParams.find((o) => o.ph === `/${part}`);
      if (opt) {
        paramNames.push(opt.name);
        regexStr += `(?:\\/(?<p_${opt.name}>[^/]+))?`;
        continue;
      }

      regexStr += "\\/";

      const cust = custom.find((c) => c.ph === part);
      if (cust) {
        paramNames.push(cust.name);
        regexStr += `(?<p_${cust.name}>${cust.regex})`;
      } else if (part === "*") {
        if (i === parts.length - 1) {
          regexStr += "(?:.*)";
        } else {
          regexStr += "(?:[^/]+)";
        }
      } else if (part.startsWith(":")) {
        const name = part.slice(1);
        paramNames.push(name);
        regexStr += `(?<p_${name}>[^/]+)`;
      } else {
        regexStr += escapeRegex(part);
      }
    }
  }

  if (isTrailingWildcard) {
    if (parts.length === 0) {
      regexStr += "(?:.*)?$";
    } else {
      regexStr += "(?:|/.*)$";
    }
  } else if (!strict) {
    if (parts.length === 0) {
      regexStr += "?$";
    } else {
      regexStr += "\\/?$";
    }
  } else {
    regexStr += "$";
  }

  const regex = new RegExp(regexStr);

  return (pathname: string) => {
    const m = regex.exec(pathname);
    if (!m) return undefined;
    const params: Record<string, string> = {};
    if (m.groups) {
      for (const name of paramNames) {
        const val = m.groups[`p_${name}`];
        if (val !== undefined) {
          params[name] = tryDecode(val);
        }
      }
    }
    return params;
  };
}

export interface Router<T> {
  name: string;
  add(method: string, path: string, handler: T): void;
  match(method: string, path: string): [T, Record<string, string>][];
}

export class SmartRouter<T> implements Router<T> {
  name = "SmartRouter";
  protected routes: {
    method: string;
    path: string;
    handler: T;
    matcher: (pathname: string) => Record<string, string> | undefined;
  }[] = [];

  add(method: string, path: string, handler: T): void {
    const normalized = path !== "*" && !path.startsWith("/") ? "/" + path : path;
    const matcher = compilePattern(normalized, true);
    this.routes.push({
      method: method.toUpperCase(),
      path: normalized,
      handler,
      matcher,
    });
  }

  match(method: string, path: string): [T, Record<string, string>][] {
    const targetMethod = method.toUpperCase();
    const res: [T, Record<string, string>][] = [];
    for (const r of this.routes) {
      if (r.method === "ALL" || r.method === targetMethod) {
        const params = r.matcher(path);
        if (params !== undefined) {
          res.push([r.handler, params]);
        }
      }
    }
    return res;
  }
}

export class RegExpRouter<T> extends SmartRouter<T> {
  override name = "RegExpRouter";
}
