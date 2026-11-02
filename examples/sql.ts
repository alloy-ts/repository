import assert from "node:assert";
import {
  integer,
  text,
  boolean,
  timestamptz,
  enumType,
  table,
  defineSchema,
} from "../dist/index.js";

console.log("1. Constructing Schema using NAPI-RS DSL...");
const userRole = enumType("user_role", ["admin", "editor", "viewer"]);

const users = table("users", {
  id: integer().primaryKey({ autoIncrement: true }).meta,
  name: text().maxLength(100).meta,
  email: text().unique().meta,
  role: { kind: "enum", name: "user_role", default: { kind: "literal", value: "viewer" } },
  active: boolean().default(true).meta,
  createdAt: timestamptz().defaultNow().meta,
});

const posts = table("posts", {
  id: integer().primaryKey({ autoIncrement: true }).meta,
  title: text().meta,
  authorId: integer().references("users.id", {
    onDelete: "CASCADE",
    onUpdate: "CASCADE",
  }).meta,
});

const schema = defineSchema({
  enums: [userRole],
  tables: [users, posts],
});

assert.strictEqual(schema.irVersion, 1);
assert.strictEqual(schema.tables.length, 2);
assert.strictEqual(schema.enums.length, 1);

console.log("Schema generated cleanly via NAPI-RS DSL:");
console.log(JSON.stringify(schema, null, 2));

console.log("\n2. Checking SQLite vs PostgreSQL expectations:");
console.log(
  "- SQLite: INTEGER PRIMARY KEY AUTOINCREMENT, CURRENT_TIMESTAMP, CHECK (role IN (...))",
);
console.log("- Postgres: SERIAL, NOW(), CREATE TYPE user_role AS ENUM (...)");

console.log("\nSQL Schema Compiler example completed successfully.");
