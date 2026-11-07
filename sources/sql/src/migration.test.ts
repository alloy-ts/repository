import { describe, expect, test } from "vite-plus/test";
import {
  generateMigration,
} from "./mod.ts";

describe("SQL schema migrations", () => {
  const v1Schema = JSON.stringify({
    irVersion: 1,
    tables: [
      {
        name: "users",
        columns: [
          { key: "id", name: "id", type: { kind: "integer" }, nullable: false, autoIncrement: true },
          { key: "name", name: "name", type: { kind: "text" }, nullable: true }
        ],
        primaryKey: ["id"],
        uniques: [],
        indexes: [],
        foreignKeys: []
      }
    ]
  });

  const v2Schema = JSON.stringify({
    irVersion: 1,
    tables: [
      {
        name: "users",
        columns: [
          { key: "id", name: "id", type: { kind: "integer" }, nullable: false, autoIncrement: true },
          { key: "name", name: "name", type: { kind: "text" }, nullable: false },
          { key: "email", name: "email", type: { kind: "text" }, nullable: false }
        ],
        primaryKey: ["id"],
        uniques: [],
        indexes: [
          { name: "users_email_idx", columns: ["email"], unique: true }
        ],
        foreignKeys: []
      }
    ]
  });

  test("generates PostgreSQL up and down migrations", () => {
    const migration = generateMigration(v1Schema, v2Schema, "postgres");
    expect(migration.changesCount).toBeGreaterThan(0);
    expect(migration.upSql).toContain("ALTER TABLE users ALTER COLUMN name SET NOT NULL;");
    expect(migration.upSql).toContain("ALTER TABLE users ADD COLUMN email TEXT NOT NULL;");
    expect(migration.upSql).toContain("CREATE UNIQUE INDEX users_email_idx ON users (email);");

    expect(migration.downSql).toContain("ALTER TABLE users ALTER COLUMN name DROP NOT NULL;");
    expect(migration.downSql).toContain("ALTER TABLE users DROP COLUMN email;");
    expect(migration.downSql).toContain("DROP INDEX users_email_idx;");
  });

  test("generates SQLite table rebuild on column type or constraint change", () => {
    const v1Sqlite = JSON.stringify({
      irVersion: 1,
      tables: [
        {
          name: "users",
          columns: [
            { key: "id", name: "id", type: { kind: "integer" }, nullable: false, autoIncrement: true },
            { key: "age", name: "age", type: { kind: "integer" }, nullable: true }
          ],
          primaryKey: ["id"]
        }
      ]
    });

    const v2Sqlite = JSON.stringify({
      irVersion: 1,
      tables: [
        {
          name: "users",
          columns: [
            { key: "id", name: "id", type: { kind: "integer" }, nullable: false, autoIncrement: true },
            { key: "age", name: "age", type: { kind: "bigint" }, nullable: true }
          ],
          primaryKey: ["id"]
        }
      ]
    });

    const migration = generateMigration(v1Sqlite, v2Sqlite, "sqlite");
    expect(migration.upSql).toContain("PRAGMA foreign_keys = OFF;");
    expect(migration.upSql).toContain("BEGIN TRANSACTION;");
    expect(migration.upSql).toContain("INSERT INTO _users_new");
    expect(migration.upSql).toContain("DROP TABLE users;");
    expect(migration.upSql).toContain("ALTER TABLE _users_new RENAME TO users;");
    expect(migration.upSql).toContain("PRAGMA foreign_key_check;");
  });
});
