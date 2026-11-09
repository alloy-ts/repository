/** Registration-order router for the Fetch-style Server.
 *
 * Patterns follow the routing forms used by the server tests: static segments,
 * `:name`, trailing `:name?`, `:name{regexp}`, a single-segment `*`, and a
 * trailing `/*` that also matches the path without the suffix.
 */

export interface MatchedRoute<T> {
  handler: T;
  params: Record<string, string>;
}

export interface Router<T> {
  readonly name: string;
  add(method: string, path: string, handler: T): void;
  match(method: string, path: string): MatchedRoute<T>[];
}

interface Token {
  kind: "literal" | "param" | "star" | "tail";
  value: string;
  name: string;
  pattern: string;
}

interface Compiled<T> {
  method: string;
  regex: RegExp;
  keys: string[];
  handler: T;
}

/** Linear regexp matcher. SmartRouter delegates here; both accept the same patterns. */
export class RegExpRouter<T = unknown> implements Router<T> {
  readonly name = "RegExpRouter";
  readonly #routes: Compiled<T>[] = [];

  add(method: string, path: string, handler: T): void {
    for (const pattern of expandOptional(path)) {
      const compiled = compile(pattern);
      this.#routes.push({ method, ...compiled, handler });
    }
  }

  match(method: string, path: string): MatchedRoute<T>[] {
    const matched: MatchedRoute<T>[] = [];
    for (const route of this.#routes) {
      if (route.method !== "ALL" && route.method !== method) continue;
      const found = route.regex.exec(path);
      if (!found) continue;
      const params: Record<string, string> = {};
      route.keys.forEach((key, index) => {
        const value = found[index + 1];
        if (value !== undefined) params[key] = tryDecode(value);
      });
      matched.push({ handler: route.handler, params });
    }
    return matched;
  }
}

/** Default router. It uses the same matcher as RegExpRouter. */
export class SmartRouter<T = unknown> implements Router<T> {
  readonly name = "SmartRouter";
  readonly #inner = new RegExpRouter<T>();

  add(method: string, path: string, handler: T): void {
    this.#inner.add(method, path, handler);
  }

  match(method: string, path: string): MatchedRoute<T>[] {
    return this.#inner.match(method, path);
  }
}

function expandOptional(path: string): string[] {
  if (!path.endsWith("?") || !path.includes(":")) return [path];
  const results: string[] = [];
  let base = "";
  for (const segment of path.split("/")) {
    if (segment === "") continue;
    if (segment.startsWith(":")) {
      if (segment.endsWith("?")) {
        results.push(base === "" ? "/" : base);
        base += `/${segment.slice(0, -1)}`;
        results.push(base);
      } else {
        base += `/${segment}`;
      }
    } else {
      base += `/${segment}`;
    }
  }
  return results.length > 0 ? [...new Set(results)] : [path];
}

function compile(pattern: string): { regex: RegExp; keys: string[] } {
  if (pattern === "*" || pattern === "/*") return { regex: /^[\s\S]*$/, keys: [] };
  const keys: string[] = [];
  let source = "";
  const tokens = tokenize(pattern);
  for (let index = 0; index < tokens.length; index++) {
    const token = tokens[index]!;
    if (token.kind === "literal") source += escapeRegExp(token.value);
    else if (token.kind === "param") {
      keys.push(token.name);
      source += `(${token.pattern})`;
    } else if (token.kind === "star") {
      source += index === tokens.length - 1 ? "[\\s\\S]*" : "[^/]+";
    } else source += "(?:|/[\\s\\S]*)";
  }
  return { regex: new RegExp(`^${source}$`), keys };
}

function tokenize(pattern: string): Token[] {
  const groups: string[] = [];
  const marked = pattern.replace(/\{(?:\{[\d,]+\}|[^}])+\}/g, (match) => {
    const mark = `\uE000${groups.length}\uE000`;
    groups.push(match);
    return mark;
  });
  const tokens: Token[] = [];
  let index = 0;
  while (index < marked.length) {
    if (marked.startsWith("/*", index) && index + 2 === marked.length) {
      tokens.push(token("tail"));
      break;
    }
    const char = marked[index]!;
    if (char === "*") {
      tokens.push(token("star"));
      index += 1;
      continue;
    }
    if (char === ":") {
      let end = index + 1;
      while (end < marked.length && marked[end] !== "/") end += 1;
      const raw = restore(marked.slice(index, end), groups);
      const match = /^:([^?{}]+)(?:\{(.+)\})?\??$/.exec(raw);
      if (!match?.[1]) throw new TypeError(`unsupported route pattern: ${pattern}`);
      const parsed = token("param");
      parsed.name = match[1];
      parsed.pattern = match[2] ?? "[^/]+";
      tokens.push(parsed);
      index = end;
      continue;
    }
    let end = index;
    while (end < marked.length) {
      if (marked.startsWith("/*", end) && end + 2 === marked.length) break;
      const current = marked[end];
      if (current === "*" || current === ":") break;
      end += 1;
    }
    const literal = token("literal");
    literal.value = restore(marked.slice(index, end), groups);
    tokens.push(literal);
    index = end;
  }
  return tokens;
}

function token(kind: Token["kind"]): Token {
  return { kind, value: "", name: "", pattern: "" };
}

function restore(value: string, groups: string[]): string {
  return value.replace(/\uE000(\d+)\uE000/g, (_mark, index: string) => groups[Number(index)] ?? "");
}

function escapeRegExp(value: string): string {
  return value.replace(/[|\\{}()[\]^$+*?.]/g, "\\$&");
}

function tryDecode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}
