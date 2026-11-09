/** Registration-order routers for Server.
 *
 * `RegExpRouter` compiles every route into one expression of lookaheads and
 * refuses sibling parameter patterns that use different expressions.
 * `TrieRouter` walks a segment trie and allows those patterns.
 * `SmartRouter` keeps the regexp router while it accepts every route, then
 * falls back to the trie. Handlers still run in registration order.
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

export class UnsupportedPathError extends Error {
  readonly path: string;
  constructor(path: string) {
    super(`unsupported route pattern: ${path}`);
    this.name = "UnsupportedPathError";
    this.path = path;
  }
}

interface Compiled<T> {
  method: string;
  source: string;
  keys: string[];
  handler: T;
  index: number;
}

interface Prepared<T> {
  expression: RegExp;
  routes: { route: Compiled<T>; params: number[]; marker: number }[];
}

/** One combined regular expression per method. Conflicting parameter patterns throw. */
export class RegExpRouter<T = unknown> implements Router<T> {
  readonly name = "RegExpRouter";
  readonly #routes: Compiled<T>[] = [];
  readonly #validators = new Map<string, ValidatorNode>();
  #prepared = new Map<string, Prepared<T>>();
  #index = 0;

  add(method: string, path: string, handler: T): void {
    const verb = method.toUpperCase();
    for (const pattern of expandOptional(path)) {
      const validator = this.#validators.get(verb) ?? new ValidatorNode();
      this.#validators.set(verb, validator);
      validator.insert(pattern);
      const compiled = compile(pattern);
      this.#routes.push({ method: verb, ...compiled, handler, index: this.#index++ });
    }
    this.#prepared.clear();
  }

  match(method: string, path: string): MatchedRoute<T>[] {
    const verb = method.toUpperCase();
    const prepared = this.#prepare(verb);
    const found = prepared.expression.exec(path);
    if (!found) return [];
    const matched: MatchedRoute<T>[] = [];
    for (const item of prepared.routes) {
      if (found[item.marker] !== "") continue;
      const params: Record<string, string> = {};
      item.route.keys.forEach((key, index) => {
        const group = item.params[index];
        const value = group === undefined ? undefined : found[group];
        if (value !== undefined) params[key] = tryDecode(value);
      });
      matched.push({ handler: item.route.handler, params });
    }
    return matched;
  }

  #prepare(method: string): Prepared<T> {
    const cached = this.#prepared.get(method);
    if (cached) return cached;
    const routes = this.#routes.filter((route) => route.method === method || route.method === "ALL");
    let nextGroup = 1;
    const parts: string[] = [];
    const preparedRoutes: Prepared<T>["routes"] = [];
    for (const route of routes) {
      const params: number[] = [];
      for (let index = 0; index < route.keys.length; index++) params.push(nextGroup++);
      const marker = nextGroup++;
      parts.push(`(?:(?=^${route.source}$)()|)`);
      preparedRoutes.push({ route, params, marker });
    }
    const expression = new RegExp(parts.join("") || "(?!)");
    const prepared = { expression, routes: preparedRoutes };
    this.#prepared.set(method, prepared);
    return prepared;
  }
}

interface TrieHandler<T> {
  method: string;
  handler: T;
  index: number;
}

interface ParamEdge<T> {
  name: string;
  source: string;
  expression: RegExp;
  spans: boolean;
  node: TrieNode<T>;
}

class TrieNode<T> {
  readonly statics = new Map<string, TrieNode<T>>();
  readonly params: ParamEdge<T>[] = [];
  wild?: TrieNode<T>;
  rest?: TrieNode<T>;
  readonly handlers: TrieHandler<T>[] = [];
}

/** Segment trie. Different parameter expressions at one segment all remain matchable. */
export class TrieRouter<T = unknown> implements Router<T> {
  readonly name = "TrieRouter";
  readonly #root = new TrieNode<T>();
  #index = 0;

  add(method: string, path: string, handler: T): void {
    const verb = method.toUpperCase();
    for (const pattern of expandOptional(path)) {
      const node = insertTrie(this.#root, pattern);
      node.handlers.push({ method: verb, handler, index: this.#index++ });
    }
  }

  match(method: string, path: string): MatchedRoute<T>[] {
    const verb = method.toUpperCase();
    const found: { index: number; handler: T; params: Record<string, string> }[] = [];
    walkTrie(this.#root, splitPath(path), 0, {}, verb, found);
    found.sort((left, right) => left.index - right.index);
    return found.map(({ handler, params }) => ({ handler, params }));
  }
}

/** Prefers RegExpRouter and drops it when a pattern cannot be combined. */
export class SmartRouter<T = unknown> implements Router<T> {
  readonly name = "SmartRouter";
  #routers: Router<T>[];

  constructor(routers?: Router<T>[]) {
    this.#routers = routers ?? [new RegExpRouter<T>(), new TrieRouter<T>()];
  }

  get active(): string {
    return this.#routers[0]?.name ?? "none";
  }

  add(method: string, path: string, handler: T): void {
    const kept: Router<T>[] = [];
    let added = false;
    for (const router of this.#routers) {
      try {
        router.add(method, path, handler);
        kept.push(router);
        added = true;
      } catch (error) {
        if (!(error instanceof UnsupportedPathError)) throw error;
      }
    }
    if (!added) throw new UnsupportedPathError(path);
    this.#routers = kept;
  }

  match(method: string, path: string): MatchedRoute<T>[] {
    return this.#routers[0]?.match(method, path) ?? [];
  }
}

class ValidatorNode {
  readonly statics = new Map<string, ValidatorNode>();
  param?: { source: string; node: ValidatorNode };
  wild?: ValidatorNode;
  rest?: ValidatorNode;

  insert(pattern: string): void {
    descend(this, pattern, routeSegments(pattern));
  }
}

function descend(root: ValidatorNode, pattern: string, parts: Segment[]): void {
  let node = root;
  for (const part of parts) {
      if (part.kind === "rest") {
        node.rest ??= new ValidatorNode();
        node = node.rest;
        continue;
      }
      if (part.kind === "wild") {
        node.wild ??= new ValidatorNode();
        node = node.wild;
        continue;
      }
      if (part.kind === "static") {
        const child = node.statics.get(part.value) ?? new ValidatorNode();
        node.statics.set(part.value, child);
        node = child;
        continue;
      }
      if (node.param && node.param.source !== part.source) {
        throw new UnsupportedPathError(pattern);
      }
      if (!node.param) {
        if (matchesEmpty(part.source) || hasCapturingGroup(part.source)) {
          throw new UnsupportedPathError(pattern);
        }
        node.param = { source: part.source, node: new ValidatorNode() };
      }
      node = node.param.node;
  }
}

type Segment =
  | { kind: "static"; value: string }
  | { kind: "param"; name: string; source: string; spans: boolean }
  | { kind: "wild" }
  | { kind: "rest" };

function insertTrie<T>(root: TrieNode<T>, pattern: string): TrieNode<T> {
  let node = root;
  for (const part of routeSegments(pattern)) {
    if (part.kind === "rest") {
      node.rest ??= new TrieNode();
      node = node.rest;
    } else if (part.kind === "wild") {
      node.wild ??= new TrieNode();
      node = node.wild;
    } else if (part.kind === "static") {
      const child = node.statics.get(part.value) ?? new TrieNode();
      node.statics.set(part.value, child);
      node = child;
    } else {
      const existing = node.params.find((edge) => edge.source === part.source && edge.name === part.name);
      if (existing) node = existing.node;
      else {
        const child = new TrieNode<T>();
        node.params.push({
          name: part.name,
          source: part.source,
          expression: new RegExp(`^(?:${part.source})$`),
          spans: part.spans,
          node: child,
        });
        node = child;
      }
    }
  }
  return node;
}

function walkTrie<T>(
  node: TrieNode<T>,
  segments: string[],
  index: number,
  params: Record<string, string>,
  method: string,
  found: { index: number; handler: T; params: Record<string, string> }[],
): void {
  const collect = (handlers: TrieHandler<T>[], captured: Record<string, string>) => {
    for (const handler of handlers) {
      if (handler.method !== "ALL" && handler.method !== method) continue;
      found.push({ index: handler.index, handler: handler.handler, params: { ...captured } });
    }
  };
  if (node.rest) collect(node.rest.handlers, params);
  if (index === segments.length) {
    collect(node.handlers, params);
    return;
  }
  const segment = segments[index]!;
  const staticChild = node.statics.get(segment);
  if (staticChild) walkTrie(staticChild, segments, index + 1, params, method, found);
  if (node.wild) walkTrie(node.wild, segments, index + 1, params, method, found);
  for (const edge of node.params) {
    if (edge.spans) {
      const last = edge.node.handlers.length > 0 && !edge.node.statics.size && !edge.node.params.length && !edge.node.wild && !edge.node.rest;
      if (last) {
        const rest = segments.slice(index).join("/");
        if (edge.expression.test(rest)) {
          walkTrie(edge.node, segments, segments.length, { ...params, [edge.name]: tryDecode(rest) }, method, found);
        }
        continue;
      }
      for (let take = 1; take <= segments.length - index; take++) {
        const chunk = segments.slice(index, index + take).join("/");
        if (!edge.expression.test(chunk)) continue;
        walkTrie(
          edge.node,
          segments,
          index + take,
          { ...params, [edge.name]: tryDecode(chunk) },
          method,
          found,
        );
      }
    } else if (edge.expression.test(segment)) {
      walkTrie(edge.node, segments, index + 1, { ...params, [edge.name]: tryDecode(segment) }, method, found);
    }
  }
}

function routeSegments(pattern: string): Segment[] {
  if (pattern === "*" || pattern === "/*") return [{ kind: "rest" }];
  const groups: string[] = [];
  let marked = pattern.replace(/\{(?:\{[\d,]+\}|[^}])+\}/g, (match) => {
    const mark = `\uE000${groups.length}\uE000`;
    groups.push(match);
    return mark;
  });
  let rest = false;
  if (marked.endsWith("/*")) {
    rest = true;
    marked = marked.slice(0, -2);
  }
  const parts = marked
    .split("/")
    .filter((part) => part !== "")
    .map((part) => restore(part, groups));
  const segments: Segment[] = parts.map((part) => classifySegment(part, parts.length));
  if (rest) segments.push({ kind: "rest" });
  return segments;
}

function classifySegment(part: string, count: number): Segment {
  if (part === "/*" || (part === "*" && count === 1)) return { kind: "rest" };
  if (part === "*") return { kind: "wild" };
  const param = /^:([^?{}]+)(?:\{(.+)\})?$/.exec(part);
  if (param?.[1]) {
    const source = param[2] ?? "[^/]+";
    return { kind: "param", name: param[1], source, spans: spansSlash(source) };
  }
  return { kind: "static", value: part };
}

function spansSlash(source: string): boolean {
  const full = new RegExp(`^(?:${source})$`);
  return full.test("/") || full.test("a/b") || full.test("a/b.png") || new RegExp(source).test("/");
}

function matchesEmpty(source: string): boolean {
  return new RegExp(`^(?:${source})$`).test("");
}

function hasCapturingGroup(source: string): boolean {
  return /(?<!\\)\((?!\?)/.test(source);
}

function splitPath(path: string): string[] {
  const query = path.indexOf("?");
  const bare = query === -1 ? path : path.slice(0, query);
  return bare.split("/").filter((part) => part !== "");
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
      } else base += `/${segment}`;
    } else base += `/${segment}`;
  }
  return results.length > 0 ? [...new Set(results)] : [path];
}

function compile(pattern: string): { source: string; keys: string[] } {
  if (pattern === "*" || pattern === "/*") return { source: "[\\s\\S]*", keys: [] };
  const keys: string[] = [];
  let source = "";
  const tokens = tokenize(pattern);
  for (let index = 0; index < tokens.length; index++) {
    const token = tokens[index]!;
    if (token.kind === "literal") source += escapeRegExp(token.value);
    else if (token.kind === "param") {
      keys.push(token.name);
      source += `(${token.pattern})`;
    } else if (token.kind === "star") source += index === tokens.length - 1 ? "[\\s\\S]*" : "[^/]+";
    else source += "(?:|/[\\s\\S]*)";
  }
  return { source, keys };
}

interface Token {
  kind: "literal" | "param" | "star" | "tail";
  value: string;
  name: string;
  pattern: string;
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
