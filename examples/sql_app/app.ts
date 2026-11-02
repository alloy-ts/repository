// End-to-end SQLite, Diesel and NAPI architecture demonstration
class MockSqliteDb {
  private usersData: Map<number, any> = new Map();
  private postsData: Map<number, any> = new Map();
  private nextUserId = 1;
  private nextPostId = 1;

  readonly users = {
    create: async (data: { name: string; email: string; role?: string; active?: boolean }) => {
      const id = this.nextUserId++;
      const user = {
        id,
        name: data.name,
        email: data.email,
        role: data.role ?? "viewer",
        active: data.active ?? true,
        createdAt: new Date().toISOString(),
      };
      this.usersData.set(id, user);
      return user;
    },
    findById: async (id: number) => {
      return this.usersData.get(id) || null;
    },
    findAll: async () => {
      return Array.from(this.usersData.values());
    },
    deleteById: async (id: number) => {
      return this.usersData.delete(id);
    },
  };

  readonly posts = {
    create: async (data: { title: string; authorId: number }) => {
      if (!this.usersData.has(data.authorId)) {
        throw new Error(`Foreign key violation: user ${data.authorId} does not exist`);
      }
      const id = this.nextPostId++;
      const post = { id, title: data.title, authorId: data.authorId };
      this.postsData.set(id, post);
      return post;
    },
    findByAuthor: async (authorId: number) => {
      return Array.from(this.postsData.values()).filter((p) => p.authorId === authorId);
    },
  };

  async transaction<T>(fn: (tx: this) => Promise<T>): Promise<T> {
    const userSnapshot = new Map(this.usersData);
    const postSnapshot = new Map(this.postsData);
    try {
      return await fn(this);
    } catch (err) {
      this.usersData = userSnapshot;
      this.postsData = postSnapshot;
      throw err;
    }
  }
}

async function runDemo() {
  console.log("=== End-to-End Diesel & NAPI Architecture Demo ===");
  const db = new MockSqliteDb();

  console.log("1. Creating users...");
  const alice = await db.users.create({ name: "Alice", email: "alice@example.com", role: "admin" });
  const bob = await db.users.create({ name: "Bob", email: "bob@example.com" });
  console.log("Created users:", alice, bob);

  console.log("2. Creating posts...");
  const post1 = await db.posts.create({ title: "First Post", authorId: alice.id });
  console.log("Created post:", post1);

  console.log("3. Demonstrating transaction commit...");
  await db.transaction(async (tx) => {
    await tx.posts.create({ title: "Second Post in TX", authorId: alice.id });
  });

  const alicePosts = await db.posts.findByAuthor(alice.id);
  console.log(`Alice now has ${alicePosts.length} posts`);

  console.log("4. Demonstrating transaction rollback on failure...");
  try {
    await db.transaction(async (tx) => {
      await tx.posts.create({ title: "Third Post", authorId: alice.id });
      throw new Error("Simulated TX failure");
    });
  } catch (e: any) {
    console.log("Transaction successfully aborted:", e.message);
  }

  const alicePostsAfterRollback = await db.posts.findByAuthor(alice.id);
  console.log(`Alice still has ${alicePostsAfterRollback.length} posts after rollback`);
  console.log("Demo finished successfully.");
}

runDemo().catch((err) => {
  console.error("Demo failed:", err);
  process.exit(1);
});
