import { describe, expect, test } from "vite-plus/test";
import { Database, QueryBuilder, OrderDirection } from "./mod.ts";

describe("src/sql/query.rs NAPI bindings for Diesel QueryDsl & RunQueryDsl", () => {
  test("QueryBuilder generates expected SQL for QueryDsl methods", () => {
    const q = new QueryBuilder("users")
      .select(["id", "name", "email"])
      .filter("active", "=", true)
      .orFilter("age", ">=", 21)
      .orderBy("name", OrderDirection.Asc)
      .thenOrderBy("id", OrderDirection.Desc)
      .limit(10)
      .offset(20);

    const sql = q.toSql();
    expect(sql).toBe(
      "SELECT id, name, email FROM users WHERE active = 1 OR age >= 21 ORDER BY name ASC, id DESC LIMIT 10 OFFSET 20",
    );
  });

  test("QueryBuilder distinct, count, join, groupBy, and having", () => {
    const q = new QueryBuilder("users")
      .distinct()
      .innerJoin("posts", "posts.author_id = users.id")
      .leftJoin("comments", "comments.post_id = posts.id")
      .groupBy(["users.id", "users.name"])
      .having("count(posts.id) > 5");

    const sql = q.toSql();
    expect(sql).toContain("SELECT DISTINCT * FROM users");
    expect(sql).toContain("INNER JOIN posts ON posts.author_id = users.id");
    expect(sql).toContain("LEFT JOIN comments ON comments.post_id = posts.id");
    expect(sql).toContain("GROUP BY users.id, users.name");
    expect(sql).toContain("HAVING count(posts.id) > 5");

    const countQ = new QueryBuilder("users").count();
    expect(countQ.toSql()).toBe("SELECT COUNT(*) FROM users");
  });

  test("Database and RunQueryDsl operations (execute, insert, load, first, getResult, getResults)", () => {
    const db = new Database(":memory:");

    db.execute(
      "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, score INTEGER DEFAULT 0);",
    );

    // Insert
    db.insert("users", { name: "Alice", score: 100 });
    db.insert("users", { name: "Bob", score: 85 });
    db.insert("users", { name: "Charlie", score: 95 });

    // Load (all)
    const qAll = new QueryBuilder("users").orderBy("score", OrderDirection.Desc);
    const allUsers = db.load(qAll);
    expect(Array.isArray(allUsers)).toBe(true);
    expect(allUsers.length).toBe(3);
    expect(allUsers[0].name).toBe("Alice");
    expect(allUsers[1].name).toBe("Charlie");
    expect(allUsers[2].name).toBe("Bob");

    // First
    const qTop = new QueryBuilder("users").orderBy("score", OrderDirection.Desc);
    const topUser = db.first(qTop);
    expect(topUser.name).toBe("Alice");

    // Find by ID
    const findQ = new QueryBuilder("users").find(2);
    const user2 = db.first(findQ);
    expect(user2.name).toBe("Bob");

    // NotFound throws error
    const findMissing = new QueryBuilder("users").find(999);
    expect(() => db.first(findMissing)).toThrow(/NotFound/);

    // getResult / getResults aliases
    const single = db.getResult(findQ);
    expect(single.name).toBe("Bob");
    const multiple = db.getResults(qAll);
    expect(multiple.length).toBe(3);
  });

  test("Transaction commit and rollback lifecycle", () => {
    const db = new Database(":memory:");
    db.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, title TEXT NOT NULL);");

    // Transaction committed
    const tx1 = db.beginTransaction();
    tx1.execute("INSERT INTO items (id, title) VALUES (1, 'Item 1');");
    tx1.commit();

    const q = new QueryBuilder("items");
    let rows = db.load(q);
    expect(rows.length).toBe(1);

    // Transaction rolled back
    const tx2 = db.beginTransaction();
    tx2.execute("INSERT INTO items (id, title) VALUES (2, 'Item 2');");
    const inTxRows = tx2.load(q);
    expect(inTxRows.length).toBe(2);
    tx2.rollback();

    rows = db.load(q);
    expect(rows.length).toBe(1);
    expect(rows[0].title).toBe("Item 1");

    // Using transaction after commit/rollback throws error
    expect(() => tx2.execute("INSERT INTO items (id, title) VALUES (3, 'Item 3');")).toThrow();
  });
});
