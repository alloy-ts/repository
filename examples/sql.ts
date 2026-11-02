import assert from "node:assert";
import {
  integer,
  text,
  boolean,
  timestamptz,
  enumType,
  table,
  defineSchema,
} from "../src/sql/dsl.ts";

console.log("1. Constructing Schema using DSL...");
const userRole = enumType("user_role", ["admin", "editor", "viewer"]);

const users = table("users", {
  id: integer().primaryKey({ autoIncrement: true }),
  name: text().maxLength(100),
  email: text().unique(),
  role: userRole().default("viewer"),
  active: boolean().default(true),
  createdAt: timestamptz().defaultNow(),
});

const posts = table("posts", {
  id: integer().primaryKey({ autoIncrement: true }),
  title: text(),
  authorId: integer().references(() => users.id, {
    onDelete: "CASCADE",
    onUpdate: "CASCADE",
  }),
});

const schema = defineSchema({
  enums: [userRole],
  tables: [users, posts],
});

assert.strictEqual(schema.irVersion, 1);
assert.strictEqual(schema.tables.length, 2);
assert.strictEqual(schema.enums.length, 1);
assert.strictEqual(schema.tables[1].foreignKeys[0].refTable, "users");
assert.deepEqual(schema.tables[1].foreignKeys[0].refColumns, ["id"]);

console.log("Schema generated cleanly via DSL:");
console.log(JSON.stringify(schema, null, 2));

console.log("\n2. Checking SQLite vs PostgreSQL expectations:");
console.log(
  "- SQLite: INTEGER PRIMARY KEY AUTOINCREMENT, CURRENT_TIMESTAMP, CHECK (role IN (...))",
);
console.log("- Postgres: SERIAL, NOW(), CREATE TYPE user_role AS ENUM (...)");

console.log("\nSQL Schema Compiler example completed successfully.");
