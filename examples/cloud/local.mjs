import { Server } from "../../dist/index.mjs";
import { app } from "./app.mjs";
const server = new Server({ host: "0.0.0.0", port: Number(process.env.PORT ?? 8080), fetch: app });
await server.start();
process.once("SIGTERM", () => {
  void server.stop();
});
process.once("SIGINT", () => {
  void server.stop();
});
