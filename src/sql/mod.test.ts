import test from "node:test";
import assert from "node:assert/strict";
import {
  defineSchema, table, enumType, integer, text, boolean, timestamptz,
} from "./dsl.js";

const throwsMsg = (fn, re) => assert.throws(fn, (e) => re.test(e.message));
const col = (ir, t, name) => ir.tables.find((x) => x.name === t).columns.find((c) => c.name === name);

test("builders are immutable", () => {
  const a = integer();
  const b = a.nullable();
  assert.equal(a.meta.nullable, false);
  assert.equal(b.meta.nullable, true);
  assert.notEqual(a, b);
});

test("builders cannot be mutated from outside", () => {
  const a = integer();
  assert.throws(() => { "use strict"; a.meta.nullable = true; });
});

test("a shared builder does not leak into other columns", () => {
  const shared = text();
  const t = table("t", { id: integer().primaryKey(), a: shared.unique(), b: shared });
  const ir = defineSchema({ tables: [t] });
  assert.equal(col(ir, "t", "b").nullable, false);
  assert.deepEqual(ir.tables[0].uniques.map((u) => u.columns), [["a"]]);
});

test("DB names are snake_case, JS keys are kept", () => {
  const t = table("t", { id: integer().primaryKey(), createdAt: timestamptz(), userID: integer() });
  const ir = defineSchema({ tables: [t] });
  const cols = ir.tables[0].columns;
  assert.deepEqual(cols.map((c) => [c.key, c.name]), [["id", "id"], ["createdAt", "created_at"], ["userID", "user_id"]]);
});

test("explicit .name() overrides snake_case", () => {
  const t = table("t", { id: integer().primaryKey(), fooBar: integer().name("legacy_col") });
  assert.equal(defineSchema({ tables: [t] }).tables[0].columns[1].name, "legacy_col");
});

test("defaults: absent, null, literal, now", () => {
  const t = table("t", {
    id: integer().primaryKey(),
    none: text(),
    nul: text().nullable().default(null),
    lit: boolean().default(false),
    now: timestamptz().defaultNow(),
  });
  const ir = defineSchema({ tables: [t] });
  assert.equal(col(ir, "t", "none").default, undefined);
  assert.deepEqual(col(ir, "t", "nul").default, { kind: "null" });
  assert.deepEqual(col(ir, "t", "lit").default, { kind: "literal", value: false });
  assert.deepEqual(col(ir, "t", "now").default, { kind: "now" });
});

test("column-level unique and pk are normalised into constraint lists", () => {
  const t = table("users", { id: integer().primaryKey({ autoIncrement: true }), email: text().unique() });
  const ir = defineSchema({ tables: [t] });
  assert.deepEqual(ir.tables[0].primaryKey, ["id"]);
  assert.deepEqual(ir.tables[0].uniques, [{ name: "users_email_key", columns: ["email"] }]);
  assert.equal(ir.tables[0].columns[0].autoIncrement, true);
});

test("composite primary key via t.primaryKey", () => {
  const t = table("m", { a: integer(), b: integer() }, (t) => [t.primaryKey(["a", "b"])]);
  assert.deepEqual(defineSchema({ tables: [t] }).tables[0].primaryKey, ["a", "b"]);
});

test("primary key declared on a column and in the table is an error", () => {
  const t = table("m", { a: integer().primaryKey(), b: integer() }, (t) => [t.primaryKey(["a", "b"])]);
  throwsMsg(() => defineSchema({ tables: [t] }), /primary key declared twice/);
});

test("index and unique names are generated, and can be overridden", () => {
  const t = table("posts", { id: integer().primaryKey(), a: integer(), b: integer() }, (t) => [
    t.index(["a"]),
    t.index(["a", "b"], { name: "custom_idx" }),
    t.unique(["b"]),
  ]);
  const ir = defineSchema({ tables: [t] });
  assert.deepEqual(ir.tables[0].indexes.map((i) => i.name), ["posts_a_idx", "custom_idx"]);
  assert.deepEqual(ir.tables[0].uniques.map((u) => u.name), ["posts_b_key"]);
});

test("index keys are translated to DB names", () => {
  const t = table("t", { id: integer().primaryKey(), createdAt: timestamptz() }, (t) => [t.index(["createdAt"])]);
  assert.deepEqual(defineSchema({ tables: [t] }).tables[0].indexes[0].columns, ["created_at"]);
});

test("index on an unknown column throws", () => {
  const t = table("x", { id: integer().primaryKey() }, (t) => [t.index(["nope"])]);
  throwsMsg(() => defineSchema({ tables: [t] }), /unknown column "nope"/);
});

test("duplicate table names throw", () => {
  const users = table("users", { id: integer().primaryKey() });
  throwsMsg(() => defineSchema({ tables: [users, users] }), /duplicate table names/);
});

test("maxLength validates its argument", () => {
  for (const bad of [0, -1, 1.5, "3", null]) throwsMsg(() => text().maxLength(bad), /positive integer/);
  assert.equal(text().maxLength(10).meta.maxLength, 10);
});

test("references: thunk resolves to a foreign key with DB names", () => {
  const users = table("users", { id: integer().primaryKey() });
  const posts = table("posts", {
    id: integer().primaryKey(),
    authorId: integer().references(() => users.id, { onDelete: "cascade", onUpdate: "restrict" }),
  });
  const ir = defineSchema({ tables: [posts, users] }); // declaration order does not matter
  assert.deepEqual(ir.tables.find((t) => t.name === "posts").foreignKeys, [{
    name: "posts_author_id_fkey", columns: ["author_id"], refTable: "users", refColumns: ["id"],
    onDelete: "cascade", onUpdate: "restrict",
  }]);
});

test("references: defaults to no_action", () => {
  const users = table("users", { id: integer().primaryKey() });
  const posts = table("posts", { id: integer().primaryKey(), u: integer().references(() => users.id) });
  const fk = defineSchema({ tables: [users, posts] }).tables[1].foreignKeys[0];
  assert.equal(fk.onDelete, "no_action");
  assert.equal(fk.onUpdate, "no_action");
});

test("references: string targets are rejected", () => {
  throwsMsg(() => integer().references("users.id"), /expects a thunk/);
});

test("references: unknown action is rejected", () => {
  throwsMsg(() => integer().references(() => null, { onDelete: "explode" }), /unknown referential action/);
});

test("references: target table missing from defineSchema throws", () => {
  const other = table("other", { id: integer().primaryKey() });
  const posts = table("posts", { id: integer().primaryKey(), o: integer().references(() => other.id) });
  throwsMsg(() => defineSchema({ tables: [posts] }), /not in defineSchema/);
});

test("references: thunk that does not return a column throws", () => {
  const posts = table("posts", { id: integer().primaryKey(), o: integer().references(() => 42) });
  throwsMsg(() => defineSchema({ tables: [posts] }), /must return a column/);
});

test("enums: duplicate values rejected, type carries the enum name", () => {
  throwsMsg(() => enumType("e", ["a", "a"]), /duplicate values/);
  const role = enumType("user_role", ["a", "b"]);
  const t = table("t", { id: integer().primaryKey(), r: role().default("a") });
  const ir = defineSchema({ enums: [role], tables: [t] });
  assert.deepEqual(ir.enums, [{ name: "user_role", values: ["a", "b"] }]);
  assert.deepEqual(col(ir, "t", "r").type, { kind: "enum", name: "user_role" });
});

test("table() rejects non-builders and reserved keys", () => {
  throwsMsg(() => table("t", { id: 5 }), /expected a column builder/);
  throwsMsg(() => table("t", { $def: integer() }), /reserved/);
});

test("table() third argument must return an array", () => {
  throwsMsg(() => table("t", { id: integer().primaryKey() }, () => ({})), /must return an array/);
});

test("IR is plain JSON: no functions, stable under a round trip", () => {
  const users = table("users", { id: integer().primaryKey() });
  const posts = table("posts", { id: integer().primaryKey(), u: integer().references(() => users.id) });
  const ir = defineSchema({ tables: [users, posts] });
  const once = JSON.stringify(ir); // undefined fields are dropped, which means "absent"
  assert.equal(JSON.stringify(JSON.parse(once)), once);
  assert.ok(!once.includes("function"));
  assert.equal(ir.irVersion, 1);
});
