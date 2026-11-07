import assert from "node:assert";
import {
  integer,
  text,
  boolean,
  timestamptz,
  enumType,
  table,
  defineSchema,
} from "../workspaces/repository/src/sql/mod.ts";

console.log("1. Constructing Schema using DSL...");
const userRole = enumType("user_role", ["admin", "editor", "viewer"]);

const users = table("users", {
  id: integer().primaryKey({ autoIncrement: true }),
  name: text().maxLength(100),
  email: text().unique(),
  active: boolean().default(true),
  createdAt: timestamptz().defaultNow(),
});

const posts = table("posts", {
  id: integer().primaryKey({ autoIncrement: true }),
  title: text(),
  authorId: integer().references({ $tableName: "users", $name: "id" }, {
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

console.log("Schema generated cleanly via DSL:");
console.log(JSON.stringify(schema, null, 2));

console.log("\n2. Checking SQLite vs PostgreSQL expectations:");
console.log("- SQLite: INTEGER PRIMARY KEY AUTOINCREMENT, CURRENT_TIMESTAMP, CHECK (role IN (...))");
console.log("- Postgres: SERIAL, NOW(), CREATE TYPE user_role AS ENUM (...)");

console.log("\nSQL Schema Compiler example completed successfully.");
