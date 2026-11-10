import { describe, expect, test } from "vite-plus/test";
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
} from "./mod.ts";

describe("ColumnBuilder NAPI bindings in Rust", () => {
  test("ColumnBuilder types and method chaining", () => {
    const idCol = integer().primaryKey({ autoIncrement: true });
    expect(idCol.meta.kind).toBe("integer");
    expect(idCol.meta.primaryKey).toBe(true);
    expect(idCol.meta.autoIncrement).toBe(true);

    const titleCol = text().name("custom_title").maxLength(200);
    expect(titleCol.meta.name).toBe("custom_title");
    expect(titleCol.meta.maxLength).toBe(200);

    const activeCol = boolean().default(true);
    expect(activeCol.meta.default).toEqual({ kind: "literal", value: true });

    const timeCol = timestamptz().defaultNow();
    expect(timeCol.meta.default).toEqual({ kind: "now" });

    const uuidCol = uuid().nullable();
    expect(uuidCol.meta.nullable).toBe(true);

    const jsonCol = json().unique();
    expect(jsonCol.meta.unique).toBe(true);
  });

  test("table and defineSchema full schema integration", () => {
    const roleEnum = enumType("role", ["admin", "user"]);
    const users = table("users", {
      id: integer().primaryKey({ autoIncrement: true }),
      name: text().maxLength(50),
    });

    const posts = table("posts", {
      id: integer().primaryKey({ autoIncrement: true }),
      authorId: integer().references({ $tableName: "users", $name: "id" }, { onDelete: "CASCADE" }),
    });

    const schema = defineSchema({
      enums: [roleEnum],
      tables: [users, posts],
    });

    expect(schema.irVersion).toBe(1);
    expect(schema.tables.length).toBe(2);
    expect(schema.enums.length).toBe(1);
    expect(schema.enums[0].name).toBe("role");
  });
});
