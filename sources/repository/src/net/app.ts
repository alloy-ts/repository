/** Portable application surface. This module must not import Node or native code. */
export interface AppContext {
  env?: unknown;
  waitUntil?: (task: Promise<unknown>) => void;
  platform?: unknown;
}
export type RoutedRequest<R extends Request = Request> = R & { params: Record<string, string> };
export type Handler<R extends Request = Request> = (
  request: RoutedRequest<R>,
  context: AppContext,
) => Response | Promise<Response>;
export type Route<R extends Request = Request> =
  | Response
  | Blob
  | Handler<R>
  | Partial<Record<string, Response | Blob | Handler<R>>>;
export interface AppOptions<R extends Request = Request> {
  routes?: Record<string, Route<R>>;
  fetch?: Handler<R>;
  /** Optional native matcher; route indices refer to registration order. */
  matchRoute?: (
    method: string,
    path: string,
  ) => { matches: { handlerId: number; params: Record<string, string> }[] };
}
export type App<R extends Request = Request> = (
  request: R,
  context?: AppContext,
) => Promise<Response>;

export function createApp<R extends Request = Request>(options: AppOptions<R> = {}): App<R> {
  const routes = options.matchRoute
    ? Object.entries(options.routes ?? {})
    : Object.entries(options.routes ?? {}).sort(([a], [b]) => score(b) - score(a));
  return async (input, context = {}) => {
    try {
      const request = input as RoutedRequest<R>;
      request.params = {};
      let selected: Route<R> | undefined;
      const nativeMatches = options.matchRoute?.(
        request.method,
        new URL(request.url).pathname,
      ).matches;
      if (nativeMatches?.length) {
        const matched = nativeMatches[0]!;
        const route = routes[matched.handlerId]?.[1];
        if (route) {
          request.params = Object.fromEntries(
            Object.entries(matched.params).map(([k, v]) => [k, decodeURIComponent(v)]),
          );
          selected = route;
        }
      }
      for (const [pattern, route] of nativeMatches ? [] : routes) {
        let params: Record<string, string> | undefined;
        try {
          params = match(pattern, new URL(request.url).pathname);
        } catch (error) {
          if (error instanceof URIError) return new Response("Bad Request", { status: 400 });
          throw error;
        }
        if (params) {
          request.params = params;
          selected = route;
          break;
        }
      }
      if (
        selected &&
        !(selected instanceof Response) &&
        !(selected instanceof Blob) &&
        typeof selected !== "function"
      ) {
        selected =
          selected[request.method] ?? (request.method === "HEAD" ? selected.GET : undefined);
      }
      const response =
        selected instanceof Blob
          ? new Response(selected.stream(), { headers: { "content-type": selected.type } })
          : typeof selected === "function"
            ? await selected(request, context)
            : selected instanceof Response
              ? selected.clone()
              : options.fetch
                ? await options.fetch(request, context)
                : new Response("Not Found", { status: 404 });
      if (!(response instanceof Response)) throw new TypeError("route must return a Response");
      if (request.method === "HEAD") {
        void response.body?.cancel().catch(() => {});
        return new Response(null, {
          status: response.status,
          statusText: response.statusText,
          headers: response.headers,
        });
      }
      return response;
    } catch {
      return new Response("Internal Server Error", { status: 500 });
    }
  };
}
function score(pattern: string): number {
  return pattern
    .split("/")
    .reduce((n, part) => n + (part === "*" ? -1000 : part.startsWith(":") ? 1 : 10), 0);
}
function match(pattern: string, path: string): Record<string, string> | undefined {
  const parts = pattern.split("/");
  const actual = path.split("/");
  const params: Record<string, string> = Object.create(null);
  for (let i = 0; i < parts.length; i++) {
    const part = parts[i]!;
    const segment = actual[i];
    if (part === "*") return params;
    if (segment === undefined) return;
    if (part.startsWith(":")) params[part.slice(1)] = decodeURIComponent(segment);
    else if (part !== segment) return;
  }
  return parts.length === actual.length ? params : undefined;
}
