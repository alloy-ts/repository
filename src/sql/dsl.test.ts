import test from "node:test";
import assert from "node:assert/strict";
import {
  integer,
  text,
  boolean,
  timestamp,
  timestamptz,
  uuid,
  json,
  enumType,
  table,
  defineSchema,
} from "./dsl.ts";

test("ColumnBuilder types and method chaining", () => {
  const idCol = integer().primaryKey({ autoIncrement: true });
  assert.equal(idCol.meta.type.kind, "integer");
  assert.equal(idCol.meta.primaryKey, true);
  assert.equal(idCol.meta.autoIncrement, true);

  const titleCol = text().name("custom_title").maxLength(200);
  assert.equal(titleCol.meta.name, "custom_title");
  assert.equal(titleCol.meta.maxLength, 200);

  const activeCol = boolean().default(true);
  assert.deepEqual(activeCol.meta.default, { kind: "literal", value: true });

  const timeCol = timestamptz().defaultNow();
  assert.deepEqual(timeCol.meta.default, { kind: "now" });

  const uuidCol = uuid().nullable();
  assert.equal(uuidCol.meta.nullable, true);

  const jsonCol = json().unique();
  assert.equal(jsonCol.meta.unique, true);
});

test("table and defineSchema full schema integration", () => {
  const roleEnum = enumType("role", ["admin", "user"]);
  const users = table("users", {
    id: integer().primaryKey({ autoIncrement: true }),
    name: text().maxLength(50),
    role: roleEnum().default("user"),
  });

  const posts = table("posts", {
    id: integer().primaryKey({ autoIncrement: true }),
    authorId: integer().references(() => users.id, { onDelete: "CASCADE" }),
  });

  const schema = defineSchema({
    enums: [roleEnum],
    tables: [users, posts],
  });

  assert.equal(schema.irVersion, 1);
  assert.equal(schema.tables.length, 2);
  assert.equal(schema.enums.length, 1);
  assert.equal(schema.enums[0].name, "role");
  assert.deepEqual(schema.tables[1].foreignKeys[0].refColumns, ["id"]);
});
