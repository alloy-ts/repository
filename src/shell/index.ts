import {
  Shell,
  type ShellConfig,
  type ShellPromiseResult,
  braces,
  escape,
  exec as napiExec,
  execSync as napiExecSync,
  execFile as napiExecFile,
  execFileSync as napiExecFileSync,
  spawn as napiSpawn,
  spawnSync as napiSpawnSync,
} from "../../dist/index.mjs";

export interface ShellPromise extends Promise<ShellPromiseResult> {
  text(): Promise<string>;
  json<T = any>(): Promise<T>;
  lines(): AsyncIterable<string>;
  blob(): Promise<Blob>;
  quiet(): ShellPromise;
  nothrow(): ShellPromise;
  throws(throwOnError: boolean): ShellPromise;
  env(variables: Record<string, string>): ShellPromise;
  cwd(directory: string): ShellPromise;
}

export class ShellCommandPromise implements ShellPromise {
  private _promise?: Promise<ShellPromiseResult>;
  private _shell: Shell;
  private _script: string;
  private _localCfg: ShellConfig = {};

  constructor(shell: Shell, script: string, localCfg: ShellConfig = {}) {
    this._shell = shell;
    this._script = script;
    this._localCfg = { ...localCfg };
  }

  private get promise(): Promise<ShellPromiseResult> {
    if (!this._promise) {
      this._promise = this.execute();
    }
    return this._promise;
  }

  private async execute(): Promise<ShellPromiseResult> {
    const res = await this._shell.run(this._script, this._localCfg);
    return res;
  }

  then<TResult1 = ShellPromiseResult, TResult2 = never>(
    onfulfilled?: ((value: ShellPromiseResult) => TResult1 | PromiseLike<TResult1>) | null,
    onrejected?: ((reason: any) => TResult2 | PromiseLike<TResult2>) | null,
  ): Promise<TResult1 | TResult2> {
    return this.promise.then(onfulfilled, onrejected);
  }

  catch<TResult = never>(
    onrejected?: ((reason: any) => TResult | PromiseLike<TResult>) | null,
  ): Promise<ShellPromiseResult | TResult> {
    return this.promise.catch(onrejected);
  }

  finally(onfinally?: (() => void) | null): Promise<ShellPromiseResult> {
    return this.promise.finally(onfinally);
  }

  quiet(): this {
    return this;
  }

  nothrow(): this {
    this._localCfg.nothrow = true;
    return this;
  }

  throws(throwOnError: boolean): this {
    this._localCfg.nothrow = !throwOnError;
    return this;
  }

  env(variables: Record<string, string>): this {
    this._localCfg.env = { ...this._localCfg.env, ...variables };
    return this;
  }

  cwd(directory: string): this {
    this._localCfg.cwd = directory;
    return this;
  }

  async text(): Promise<string> {
    this.quiet();
    const res = await this;
    return res.stdout.toString("utf8");
  }

  async json<T = any>(): Promise<T> {
    const t = await this.text();
    return JSON.parse(t);
  }

  async *lines(): AsyncIterable<string> {
    const t = await this.text();
    const split = t.split(/\r?\n/);
    if (split.length > 0 && split[split.length - 1] === "") {
      split.pop();
    }
    for (const line of split) {
      yield line;
    }
  }

  async blob(): Promise<Blob> {
    this.quiet();
    const res = await this;
    return new Blob([new Uint8Array(res.stdout)], { type: "text/plain" });
  }

  readonly [Symbol.toStringTag] = "ShellCommandPromise";
}

const defaultShell = new Shell();

export interface ShellFunction {
  (strings: TemplateStringsArray | string, ...values: any[]): ShellCommandPromise;
  cwd(path?: string): string | null;
  env(variables?: Record<string, string>): Record<string, string> | null;
  nothrow(): void;
  throws(throwOnError: boolean): void;
  braces(pattern: string): string[];
  escape(s: string): string;
}

function formatCommand(strings: TemplateStringsArray | string, values: any[]): string {
  if (typeof strings === "string") {
    return strings;
  }
  let cmd = "";
  for (let i = 0; i < strings.length; i++) {
    cmd += strings[i];
    if (i < values.length) {
      const val = values[i];
      if (val && typeof val === "object" && "raw" in val) {
        cmd += String(val.raw);
      } else if (typeof val === "string") {
        cmd += escape(val);
      } else {
        cmd += String(val);
      }
    }
  }
  return cmd;
}

export const $: ShellFunction = function $(
  strings: TemplateStringsArray | string,
  ...values: any[]
): ShellCommandPromise {
  const script = formatCommand(strings, values);
  return new ShellCommandPromise(defaultShell, script);
} as any;

$.cwd = (path?: string) => defaultShell.cwd(path);
$.env = (variables?: Record<string, string>) => defaultShell.env(variables);
$.nothrow = () => defaultShell.nothrow();
$.throws = (throwOnError: boolean) => defaultShell.throws(throwOnError);
$.braces = (pattern: string) => braces(pattern);
$.escape = (s: string) => escape(s);

// Child process compatibility exports
export const exec = napiExec;
export const execSync = napiExecSync;
export const execFile = napiExecFile;
export const execFileSync = napiExecFileSync;
export const spawn = napiSpawn;
export const spawnSync = napiSpawnSync;

export { Shell, braces, escape };
