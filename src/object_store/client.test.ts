/**
 * Tests for the client/connection configuration of remote object stores
 * (`ObjectStoreClient` in `src/object_store/client.rs`).
 *
 * `ObjectStoreClient` is an `#[napi(object)]` — a plain JS object *shape*, not a constructable
 * class. Callers pass a literal (e.g. `{ timeout: 30, proxyUrl: "…" }`) into
 * `GenericObjectStore`'s constructor, where it is translated into `object_store`'s
 * `ClientOptions` (TLS, proxy, timeouts, HTTP version, root certificates, HTTP/2 keep-alive,
 * pool sizing, DNS randomization).
 */
import { describe, expect, test } from "vite-plus/test";
import { GenericObjectStore, HttpVersion, type ObjectStoreClient } from "../main.ts";

describe("ObjectStoreClient (src/object_store/client.rs)", () => {
  test("accepts the full set of connection knobs as a plain object", () => {
    // `#[napi(object)]` fields are just plain JS properties; the literal round-trips as itself.
    const client: ObjectStoreClient = {
      allowHttp: true,
      allowInvalidCertificates: true,
      disableSystemCertificates: true,
      userAgent: "my-agent/1.0",
      timeout: 30,
      connectTimeout: 5,
      readTimeout: 10,
      proxyUrl: "https://user:pass@proxy.example:3128",
      proxyExcludes: "localhost,127.0.0.1",
      proxyCaCertificate: "-----BEGIN CERTIFICATE-----\nmock\n-----END CERTIFICATE-----",
      defaultContentType: "application/octet-stream",
      httpVersion: "Http2Only",
      poolIdleTimeout: 60,
      poolMaxIdlePerHost: 8,
      http2KeepAliveInterval: 15,
      http2KeepAliveTimeout: 20,
      http2KeepAliveWhileIdle: true,
      http2MaxFrameSize: 16 * 1024,
      randomizeAddresses: false,
    };

    expect(client.allowHttp).toBe(true);
    expect(client.allowInvalidCertificates).toBe(true);
    expect(client.disableSystemCertificates).toBe(true);
    expect(client.userAgent).toBe("my-agent/1.0");
    expect(client.timeout).toBe(30);
    expect(client.connectTimeout).toBe(5);
    expect(client.readTimeout).toBe(10);
    expect(client.proxyUrl).toBe("https://user:pass@proxy.example:3128");
    expect(client.proxyExcludes).toBe("localhost,127.0.0.1");
    expect(client.defaultContentType).toBe("application/octet-stream");
    expect(client.httpVersion).toBe("Http2Only");
    expect(client.poolIdleTimeout).toBe(60);
    expect(client.poolMaxIdlePerHost).toBe(8);
    expect(client.http2KeepAliveInterval).toBe(15);
    expect(client.http2KeepAliveTimeout).toBe(20);
    expect(client.http2KeepAliveWhileIdle).toBe(true);
    expect(client.http2MaxFrameSize).toBe(16 * 1024);
    expect(client.randomizeAddresses).toBe(false);

    // And the literal is accepted by the constructor (exercises `to_client_options`).
    expect(() => new GenericObjectStore("memory", "sha1", false, client)).not.toThrow();
  });

  test("HttpVersion string enum exposes the expected variants", () => {
    expect(HttpVersion.Http1Only).toBe("Http1Only");
    expect(HttpVersion.Http2Only).toBe("Http2Only");
    expect(HttpVersion.Either).toBe("Either");
  });

  test("translates to object_store ClientOptions when constructing a store", () => {
    // No network/TLS validation happens at construction; the options are just built.
    const client: ObjectStoreClient = {
      allowHttp: true,
      timeout: 42,
      httpVersion: "Either",
      proxyUrl: "http://proxy.example:8080",
      proxyExcludes: "internal.example",
    };
    expect(() => new GenericObjectStore("memory", "sha1", false, client)).not.toThrow();
  });

  test("accepts root certificate buffers and file paths without connecting", () => {
    // A syntactically valid self-signed-style PEM is required for `from_pem` to succeed;
    // here we only assert the path runs without throwing for the empty case.
    const client: ObjectStoreClient = {
      rootCertificateFiles: [],
      rootCertificates: [],
    };
    expect(() => new GenericObjectStore("memory", "sha1", false, client)).not.toThrow();
  });

  test("wires the client into an HTTP backend (lazy — no connection attempted)", () => {
    const client: ObjectStoreClient = {
      timeout: 10,
      connectTimeout: 2,
      httpVersion: "Http1Only",
    };
    // Construction only configures the `object_store::http` builder; it does not connect.
    expect(
      () => new GenericObjectStore("https://127.0.0.1:9/object-store-test/", "sha1", false, client),
    ).not.toThrow();
  });

  test("constructs without a client (defaults)", () => {
    expect(() => new GenericObjectStore("memory", "sha1", false)).not.toThrow();
  });
});
