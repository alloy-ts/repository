// Example default schema definition for testing generation
import { defineSchema, table, integer, text, boolean, timestamptz } from "./src/sql/dsl.ts";

export const users = table(
  {
    id: integer().primaryKey().autoIncrement(),
    name: text().maxLength(100),
    email: text().unique(),
    active: boolean().default(true),
    createdAt: timestamptz().defaultNow(),
  }
);

export const posts = table(
  {
    id: integer().primaryKey().autoIncrement(),
    title: text(),
    authorId: integer().references(() => users.id, { onDelete: "cascade" }),
  }
);

export default defineSchema({
  tables: {
    users,
    posts,
  },
});
