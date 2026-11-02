#!/usr/bin/env node
import { existsSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { resolve, dirname, relative } from "node:path";
import { pathToFileURL } from "node:url";

function parseArgs() {
  const args = process.argv.slice(2);
  let schemaPath = "schema.js";
  let outDir = "src/generated";
  let backend = "sqlite";

  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--schema" || args[i] === "-s") {
      schemaPath = args[++i];
    } else if (args[i] === "--out" || args[i] === "-o") {
      outDir = args[++i];
    } else if (args[i] === "--backend" || args[i] === "-b") {
      backend = args[++i];
    }
  }

  return { schemaPath, outDir, backend };
}

function printCargoWarnings(errors) {
  for (const err of errors) {
    const tableInfo = err.table ? ` [table: ${err.table}]` : "";
    const colInfo = err.column ? ` [column: ${err.column}]` : "";
    console.warn(`cargo:warning=Schema validation error${tableInfo}${colInfo}: ${err.message || err}`);
  }
}

async function loadSchemaIr(schemaPath) {
  // If SCHEMA_IR_JSON environment variable is provided, parse directly without evaluating JS
  if (process.env.SCHEMA_IR_JSON) {
    try {
      return JSON.parse(process.env.SCHEMA_IR_JSON);
    } catch (e) {
      throw new Error(`Failed to parse SCHEMA_IR_JSON: ${e.message}`);
    }
  }

  const resolved = resolve(process.cwd(), schemaPath);
  if (!existsSync(resolved)) {
    // Check if schema.ts exists
    const tsCandidate = resolved.replace(/\.js$/, ".ts");
    if (existsSync(tsCandidate)) {
      return loadSchemaIr(tsCandidate);
    }
    throw new Error(`Schema file not found at: ${resolved}`);
  }

  try {
    const url = pathToFileURL(resolved).href;
    const mod = await import(url);
    const schema = mod.default || mod.schema;
    if (!schema) {
      throw new Error(`No default export or 'schema' export found in ${schemaPath}`);
    }
    return typeof schema.toIr === "function" ? schema.toIr() : schema;
  } catch (err) {
    const rel = relative(process.cwd(), resolved);
    // Print friendly error with file/line reference
    if (err.stack) {
      const match = err.stack.match(new RegExp(`${rel}:(\\d+):(\\d+)`));
      if (match) {
        throw new Error(`Error in ${rel} at line ${match[1]}, column ${match[2]}: ${err.message}`);
      }
    }
    throw new Error(`Error executing schema definition (${rel}): ${err.message}`);
  }
}

async function main() {
  const { schemaPath, outDir, backend } = parseArgs();

  let ir;
  try {
    ir = await loadSchemaIr(schemaPath);
  } catch (e) {
    console.error(`generate.mjs failed: ${e.message}`);
    process.exit(1);
  }

  const jsonIr = JSON.stringify(ir);

  // Dynamically load native compiler binding if available
  let compiler;
  try {
    compiler = await import("../dist/index.js");
  } catch {
    // If running without prebuilt dist/, check if we can execute via child_process cargo or fall back
  }

  mkdirSync(outDir, { recursive: true });

  if (compiler && typeof compiler.compileSchema === "function") {
    try {
      const result = compiler.compileSchema(jsonIr, backend);
      writeFileSync(resolve(outDir, "models.rs"), result.modelsRust || "");
      writeFileSync(resolve(outDir, "schema.rs"), result.dieselSchema || "");
      writeFileSync(resolve(outDir, "schema.sql"), result.sqlDdl || "");
      writeFileSync(resolve(outDir, "napi.rs"), result.napiRs || "");
      writeFileSync(resolve(outDir, "validators.rs"), result.validators || "");
      writeFileSync(resolve(outDir, "schema.ir.json"), JSON.stringify(ir, null, 2));
      console.log(`Generated code successfully written to ${outDir}`);
      return;
    } catch (e) {
      console.error(`Compiler execution error: ${e.message}`);
      process.exit(1);
    }
  }

  // Write the validated schema IR snapshot
  writeFileSync(resolve(outDir, "schema.ir.json"), JSON.stringify(ir, null, 2));
  console.log(`Generated IR snapshot written to ${resolve(outDir, "schema.ir.json")}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
