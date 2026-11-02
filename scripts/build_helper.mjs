import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

/// cargo:warning formatting helper
export function printCargoWarning(message, table, column) {
  const parts = [];
  if (table) parts.push(`table: ${table}`);
  if (column) parts.push(`column: ${column}`);
  const details = parts.length > 0 ? ` [${parts.join(", ")}]` : "";
  console.warn(`cargo:warning=Schema validation warning${details}: ${message}`);
}

/// Run code generation using SCHEMA_IR_JSON if set, or a schema definition file
export function resolveIrInput(schemaPath = "schema.js") {
  if (process.env.SCHEMA_IR_JSON) {
    try {
      return JSON.parse(process.env.SCHEMA_IR_JSON);
    } catch (e) {
      throw new Error(`Invalid SCHEMA_IR_JSON environment variable: ${e.message}`);
    }
  }

  const p = resolve(process.cwd(), schemaPath);
  if (!existsSync(p)) {
    return null;
  }
  return null;
}
