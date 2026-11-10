export interface Router<T> {
  name: string;
  add(method: string, path: string, handler: T): void;
  match(method: string, path: string): [T, Record<string, string>][];
}

export class SmartRouter<T> implements Router<T> {
  name = "SmartRouter";
  #routes: { method: string; path: string; handler: T }[] = [];

  add(method: string, path: string, handler: T): void {
    this.#routes.push({ method, path, handler });
  }

  match(method: string, path: string): [T, Record<string, string>][] {
    const res: [T, Record<string, string>][] = [];
    for (const r of this.#routes) {
      if (r.method === "ALL" || r.method === method) {
        if (r.path === path || r.path === "*") {
          res.push([r.handler, {}]);
        }
      }
    }
    return res;
  }
}

export class RegExpRouter<T> extends SmartRouter<T> {
  override name = "RegExpRouter";
}
