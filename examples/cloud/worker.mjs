import { cloudflare } from "../../dist/cloud.mjs";
import { app } from "./app.mjs";
export default cloudflare(app);
