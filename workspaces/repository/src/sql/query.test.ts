import { describe, expect, test } from "vite-plus/test";
import { SqlConnection } from "../main.ts";

describe("SQL QueryDsl and RunQueryDsl (src/sql/query.rs)", () => {
  function setupTestDb() {
    const conn = new SqlConnection(":memory:");
    conn.executeBatch(`
      CREATE TABLE users (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        email TEXT,
        age INTEGER,
        is_active INTEGER DEFAULT 1
      );
    `);
    return conn;
  }

  test("insert and load records (QueryDsl + RunQueryDsl::load)", () => {
    const conn = setupTestDb();
    const query = conn.query("users");

    expect(query.insert({ name: "Alice", email: "alice@example.com", age: 30, is_active: 1 })).toBe(1);
    expect(query.insert({ name: "Bob", email: "bob@example.com", age: 25, is_active: 0 })).toBe(1);
    expect(query.insert({ name: "Charlie", email: null, age: 35, is_active: 1 })).toBe(1);

    const rows = query.load() as any[];
    expect(rows.length).toBe(3);
    expect(rows[0].name).toBe("Alice");
    expect(rows[1].name).toBe("Bob");
    expect(rows[2].name).toBe("Charlie");
  });

  test("select specific columns", () => {
    const conn = setupTestDb();
    const query = conn.query("users");
    query.insert({ name: "Alice", email: "alice@example.com", age: 30 });

    const selected = query.select(["id", "name"]).load() as any[];
    expect(selected.length).toBe(1);
    expect(selected[0].id).toBe(1);
    expect(selected[0].name).toBe("Alice");
    expect(selected[0].email).toBeUndefined();
  });

  test("filters (filterEq, filterGt, filterLt, filterLike, filterIn, filterIsNull)", () => {
    const conn = setupTestDb();
    const query = conn.query("users");
    query.insert({ name: "Alice", email: "alice@example.com", age: 30 });
    query.insert({ name: "Bob", email: "bob@example.com", age: 25 });
    query.insert({ name: "Charlie", email: null, age: 35 });

    // filterEq
    const eqRows = query.filterEq("name", "Alice").load() as any[];
    expect(eqRows.length).toBe(1);
    expect(eqRows[0].name).toBe("Alice");

    // filterGt
    const gtRows = query.filterGt("age", 25).load() as any[];
    expect(gtRows.length).toBe(2);

    // filterLike
    const likeRows = query.filterLike("name", "%li%").load() as any[];
    expect(likeRows.length).toBe(2); // Alice, Charlie

    // filterIn
    const inRows = query.filterIn("name", ["Alice", "Bob"]).load() as any[];
    expect(inRows.length).toBe(2);

    // filterIsNull
    const nullRows = query.filterIsNull("email").load() as any[];
    expect(nullRows.length).toBe(1);
    expect(nullRows[0].name).toBe("Charlie");

    // filterIsNotNull
    const notNullRows = query.filterIsNotNull("email").load() as any[];
    expect(notNullRows.length).toBe(2);
  });

  test("order_by, limit, and offset", () => {
    const conn = setupTestDb();
    const query = conn.query("users");
    query.insert({ name: "UserA", age: 10 });
    query.insert({ name: "UserB", age: 20 });
    query.insert({ name: "UserC", age: 30 });

    const descRows = query.orderByDesc("age").load() as any[];
    expect(descRows[0].name).toBe("UserC");
    expect(descRows[1].name).toBe("UserB");
    expect(descRows[2].name).toBe("UserA");

    const paged = query.orderByAsc("age").limit(1).offset(1).load() as any[];
    expect(paged.length).toBe(1);
    expect(paged[0].name).toBe("UserB");
  });

  test("RunQueryDsl: first and getResult", () => {
    const conn = setupTestDb();
    const query = conn.query("users");

    expect(query.first()).toBeNull();
    expect(() => query.getResult()).toThrow();

    query.insert({ name: "SoleUser", age: 40 });
    const firstUser = query.first() as any;
    expect(firstUser).not.toBeNull();
    expect(firstUser.name).toBe("SoleUser");

    const singleUser = query.getResult() as any;
    expect(singleUser.name).toBe("SoleUser");
  });

  test("count and exists", () => {
    const conn = setupTestDb();
    const query = conn.query("users");

    expect(query.count()).toBe(0);
    expect(query.exists()).toBe(false);

    query.insert({ name: "Test1", age: 20 });
    expect(query.count()).toBe(1);
    expect(query.exists()).toBe(true);

    expect(query.filterEq("name", "NonExistent").exists()).toBe(false);
  });

  test("update and delete operations", () => {
    const conn = setupTestDb();
    const query = conn.query("users");
    query.insert({ name: "ToUpdate", age: 20, is_active: 1 });
    query.insert({ name: "ToDelete", age: 30, is_active: 0 });

    const updated = query.filterEq("name", "ToUpdate").update({ age: 21, is_active: 0 });
    expect(updated).toBe(1);

    const checkUpdated = query.filterEq("name", "ToUpdate").first() as any;
    expect(checkUpdated.age).toBe(21);
    expect(checkUpdated.is_active).toBe(0);

    const deleted = query.filterEq("name", "ToDelete").delete();
    expect(deleted).toBe(1);
    expect(query.count()).toBe(1);
  });

  test("toSql renders valid SQL debug string", () => {
    const conn = setupTestDb();
    const query = conn.query("users")
      .select(["id", "name"])
      .filterEq("is_active", 1)
      .orderByDesc("id")
      .limit(10)
      .offset(5);

    const sql = query.toSql();
    expect(sql).toContain("SELECT id, name FROM users");
    expect(sql).toContain("WHERE is_active = 1");
    expect(sql).toContain("ORDER BY id DESC");
    expect(sql).toContain("LIMIT 10");
    expect(sql).toContain("OFFSET 5");
  });
});
