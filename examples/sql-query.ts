import { SqlConnection } from "../sources/repository/src/main.ts";

function main() {
  console.log("=== Diesel QueryDsl and RunQueryDsl Example (examples/sql-query.ts) ===");

  // 1. Establish SQLite connection
  const conn = new SqlConnection(":memory:");
  console.log("Established SQLite in-memory connection.");

  // 2. Set up schema
  conn.executeBatch(`
    CREATE TABLE employees (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      name TEXT NOT NULL,
      department TEXT NOT NULL,
      salary INTEGER NOT NULL,
      active INTEGER DEFAULT 1
    );
  `);
  console.log("Created table 'employees'.");

  // 3. Insert records (InsertStatement)
  const q = conn.query("employees");
  q.insert({ name: "Alice", department: "Engineering", salary: 120000, active: 1 });
  q.insert({ name: "Bob", department: "Engineering", salary: 95000, active: 1 });
  q.insert({ name: "Charlie", department: "Product", salary: 110000, active: 1 });
  q.insert({ name: "Diana", department: "Design", salary: 85000, active: 0 });
  console.log("Inserted 4 employee records.");

  // 4. QueryDsl: Select, Filter, Order, Limit, Offset
  console.log("\n--- QueryDsl chaining & RunQueryDsl.load() ---");
  const engineering = q
    .filterEq("department", "Engineering")
    .filterGt("salary", 90000)
    .orderByDesc("salary")
    .load() as any[];

  console.log(`Found ${engineering.length} engineers with salary > $90k:`);
  for (const emp of engineering) {
    console.log(`  - ${emp.name}: $${emp.salary} (${emp.department})`);
  }

  // 5. QueryDsl.select(): picking specific columns
  console.log("\n--- QueryDsl.select() ---");
  const namesAndDepts = q.select(["name", "department"]).limit(2).load() as any[];
  console.log("Selected first 2 employees (name, department only):");
  console.log(namesAndDepts);

  // 6. RunQueryDsl: first() and getResult()
  console.log("\n--- RunQueryDsl.first() & getResult() ---");
  const topEarner = q.orderByDesc("salary").first() as any;
  console.log(`Top earner via first(): ${topEarner?.name} ($${topEarner?.salary})`);

  const singleEmp = q.filterEq("name", "Charlie").getResult() as any;
  console.log(`Single result via getResult(): ${singleEmp?.name} in ${singleEmp?.department}`);

  // 7. Aggregates: count() and exists()
  console.log("\n--- Aggregates: count() & exists() ---");
  console.log(`Total active employees count: ${q.filterEq("active", 1).count()}`);
  console.log(`Has any employees in Sales? ${q.filterEq("department", "Sales").exists()}`);
  console.log(
    `Has any employees in Engineering? ${q.filterEq("department", "Engineering").exists()}`,
  );

  // 8. Debug SQL representation (toSql)
  console.log("\n--- toSql() generated query ---");
  const debugQuery = q
    .filterEq("department", "Engineering")
    .filterGte("salary", 100000)
    .orderByAsc("name")
    .limit(5);
  console.log(`Rendered SQL:\n  ${debugQuery.toSql()}`);

  // 9. Update & Delete operations
  console.log("\n--- Update & Delete operations ---");
  const updatedCount = q.filterEq("name", "Bob").update({ salary: 105000 });
  console.log(
    `Updated ${updatedCount} row(s). Bob's new salary:`,
    q.filterEq("name", "Bob").first(),
  );

  const deletedCount = q.filterEq("active", 0).delete();
  console.log(`Deleted ${deletedCount} inactive employee(s). Total remaining: ${q.count()}`);

  console.log("\nDiesel Query DSL example completed successfully!");
}

main();
