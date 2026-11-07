import { describe, expect, test } from "vite-plus/test";
import { formatTrailer, formatTrailers, messageTrailers, Trailer } from "../../repository/src/main.ts";

describe("Trailer class and utilities (src/repo/trailer.rs)", () => {
  test("Trailer constructor, properties, format()", () => {
    const t = new Trailer("Co-authored-by", "Alice <alice@example.com>");
    expect(t.key).toBe("Co-authored-by");
    expect(t.token).toBe("Co-authored-by");
    expect(t.value).toBe("Alice <alice@example.com>");
    expect(t.format()).toBe("Co-authored-by: Alice <alice@example.com>");
    expect(t.format(" = ")).toBe("Co-authored-by = Alice <alice@example.com>");

    t.key = "Reviewed-by";
    expect(t.key).toBe("Reviewed-by");
    expect(t.token).toBe("Reviewed-by");

    t.token = "Signed-off-by";
    expect(t.key).toBe("Signed-off-by");
    expect(t.token).toBe("Signed-off-by");

    t.value = "Bob <bob@example.com>";
    expect(t.value).toBe("Bob <bob@example.com>");
  });

  test("Trailer.parseDescriptionTrailers", () => {
    const msg = [
      "chore: update dependencies",
      "",
      "Some description paragraph.",
      "",
      "Co-authored-by: Alice <alice@example.com>",
      "Reviewed-by: Bob <bob@example.com>",
      "Change-Id: I1234567890abcdef",
    ].join("\n");

    const trailers = Trailer.parseDescriptionTrailers(msg);
    expect(trailers.length).toBe(3);
    expect(trailers[0].key).toBe("Co-authored-by");
    expect(trailers[0].value).toBe("Alice <alice@example.com>");
    expect(trailers[1].key).toBe("Reviewed-by");
    expect(trailers[1].value).toBe("Bob <bob@example.com>");
    expect(trailers[2].key).toBe("Change-Id");
    expect(trailers[2].value).toBe("I1234567890abcdef");
  });

  test("Trailer.parseTrailers", () => {
    const block = "foo: 1\nbar: 2\n";
    const trailers = Trailer.parseTrailers(block);
    expect(trailers.length).toBe(2);
    expect(trailers[0].key).toBe("foo");
    expect(trailers[0].value).toBe("1");
    expect(trailers[1].key).toBe("bar");
    expect(trailers[1].value).toBe("2");

    expect(() => Trailer.parseTrailers("foo: 1\n\nbar: 2\n")).toThrow();
  });

  test("messageTrailers function", () => {
    const msg = "chore: update\n\nSigned-off-by: Test <test@example.com>\n---\ncut content";
    const trailers = messageTrailers(msg);
    expect(trailers.length).toBe(1);
    expect(trailers[0].key).toBe("Signed-off-by");
    expect(trailers[0].value).toBe("Test <test@example.com>");
  });

  test("formatTrailer and formatTrailers with Trailer instances and objects", () => {
    const t = new Trailer("Signed-off-by", "A <a@e.com>");
    expect(formatTrailer(t)).toBe("Signed-off-by: A <a@e.com>");
    expect(formatTrailer({ key: "Signed-off-by", value: "A <a@e.com>" })).toBe(
      "Signed-off-by: A <a@e.com>",
    );
    expect(formatTrailer({ token: "Signed-off-by", value: "A <a@e.com>" })).toBe(
      "Signed-off-by: A <a@e.com>",
    );

    const list = [
      t,
      { token: "Reviewed-by", value: "B <b@e.com>" },
      { key: "Change-Id", value: "123" },
    ];
    expect(formatTrailers(list)).toBe(
      "Signed-off-by: A <a@e.com>\nReviewed-by: B <b@e.com>\nChange-Id: 123",
    );
  });
});
