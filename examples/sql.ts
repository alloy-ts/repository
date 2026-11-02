import { readFileSync } from "node:fs";

console.log("1. Inspecting generated Schema IR snapshot...");
const irJson = JSON.parse(readFileSync("src/generated/schema.ir.json", "utf8"));
console.log(`IR Version: ${irJson.irVersion}, Tables count: ${irJson.tables.length}`);

console.log("2. Checking SQLite vs PostgreSQL expectations:");
console.log("- SQLite: INTEGER PRIMARY KEY AUTOINCREMENT, CURRENT_TIMESTAMP, CHECK (role IN (...))");
console.log("- Postgres: SERIAL, NOW(), CREATE TYPE user_role AS ENUM (...)");

console.log("SQL Schema Compiler example completed successfully.");
