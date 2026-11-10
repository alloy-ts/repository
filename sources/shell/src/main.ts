export * from "../../repository/src/main.ts";
export { $, ShellCommandPromise, Shell, braces, escape } from "./index.ts";
export type { ShellPromise, ShellFunction } from "./index.ts";
export {
  exec as shellExec,
  execSync as shellExecSync,
  execFile as shellExecFile,
  execFileSync as shellExecFileSync,
  spawn as shellSpawn,
  spawnSync as shellSpawnSync,
} from "./index.ts";
