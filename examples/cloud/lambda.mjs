import { lambda } from "../../dist/cloud.mjs";
import { app } from "./app.mjs";
export const handler = lambda(app);
