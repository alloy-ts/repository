import { expect, test } from "vite-plus/test";
import { Server, type Context } from "./server.ts";

type Bindings = { TOKEN: string };
type Variables = { user: { id: string } };
type AppEnv = { Bindings: Bindings; Variables: Variables };

type Expect<T extends true> = T;
type Equal<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;

type EnvMatches = Expect<Equal<Context<AppEnv>["env"], Bindings>>;
type UserMatches = Expect<Equal<ReturnType<Context<AppEnv>["get"]>, Variables["user"]>>;

function typed(server: Server<AppEnv>): void {
  server.use("/auth/*", async (c, next) => {
    const token: string = c.env.TOKEN;
    c.set("user", { id: token });
    await next();
  });
  server.get("/auth/me", (c) => c.text(c.get("user").id));
}

function rejected(c: Context<AppEnv>): void {
  // @ts-expect-error user must be `{ id: string }`
  c.set("user", "nope");
  // @ts-expect-error `missing` is not a variable key
  c.set("missing", true);
  // @ts-expect-error TOKEN is a string
  c.env.TOKEN.toFixed();
}

test("Server generics describe bindings and variables", () => {
  const server = new Server<AppEnv>();
  typed(server);
  const envMatches = true as EnvMatches;
  const userMatches = true as UserMatches;
  expect(envMatches && userMatches).toBe(true);
  expect(rejected).toBeTypeOf("function");
  expect(server).toBeInstanceOf(Server);
});
