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
  compileSchema,
  validateSchemaJson,
} from "../main.ts";

describe("SQL DSL ColumnBuilder via Rust NAPI", () => {
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

    const tsCol = timestamp();
    expect(tsCol.meta.kind).toBe("timestamp");

    const timeCol = timestamptz().defaultNow();
    expect(timeCol.meta.default).toEqual({ kind: "now" });

    const uuidCol = uuid().nullable();
    expect(uuidCol.meta.nullable).toBe(true);

    const jsonCol = json().unique();
    expect(jsonCol.meta.unique).toBe(true);
  });

  test("table and defineSchema full schema integration and compiler validation", () => {
    const roleEnum = enumType("role", ["admin", "user"]);
    expect(roleEnum.name).toBe("role");
    expect(roleEnum.values).toEqual(["admin", "user"]);

    const users = table("users", {
      id: integer().primaryKey({ autoIncrement: true }),
      name: text().maxLength(50),
      role: { kind: "enum", enumName: "role", default: { kind: "literal", value: "user" } },
    });

    const posts = table("posts", {
      id: integer().primaryKey({ autoIncrement: true }),
      authorId: integer().references("users.id", { onDelete: "CASCADE" }),
    });

    const schema = defineSchema({
      enums: [roleEnum],
      tables: [users, posts],
    });

    expect(schema.irVersion).toBe(1);
    expect(schema.tables.length).toBe(2);
    expect(schema.enums.length).toBe(1);
    expect(schema.enums[0].name).toBe("role");

    // Verify end-to-end with compiler and validator
    const schemaJson = JSON.stringify(schema);
    const valid = validateSchemaJson(schemaJson, "sqlite");
    expect(valid).toBe(true);

    const artifacts = compileSchema(schemaJson, "sqlite");
    expect(artifacts.rustModels).toContain("pub struct User");
    expect(artifacts.rustModels).toContain("pub struct Post");
    expect(artifacts.dieselSchema).toContain("diesel::table!");
    expect(artifacts.sqlDdl).toContain("CREATE TABLE users");
    expect(artifacts.sqlDdl).toContain("CREATE TABLE posts");
  });
});
