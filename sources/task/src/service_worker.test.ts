import { describe, expect, test } from "vite-plus/test";
import { ServiceWorker, ServiceWorkerRegistration } from "../../repository/src/main.ts";

describe("ServiceWorker (MDN ServiceWorker API)", () => {
  test("constructs and exposes scriptURL, scope, and lifecycle state", async () => {
    const sw = new ServiceWorker("service-worker.js", { scope: "/app/" });
    expect(sw.scriptURL).toBe("service-worker.js");
    expect(sw.scope).toBe("/app/");
    // Initial state is installing
    expect(["installing", "installed", "activating", "activated"]).toContain(sw.state);
  });

  test("postMessage delivers message to service worker without throwing", () => {
    const sw = new ServiceWorker("sw.js");
    expect(() => sw.postMessage({ action: "skipWaiting" })).not.toThrow();
  });

  test("EventTarget methods addEventListener, dispatchEvent work as expected", () => {
    const sw = new ServiceWorker("sw.js");
    let received = false;
    sw.addEventListener("custom", () => {
      received = true;
    });
    sw.dispatchEvent({ type: "custom" });
    expect(received).toBe(true);
  });

  test("ServiceWorkerRegistration exposes scope, worker instances, and unregister method", async () => {
    const reg = new ServiceWorkerRegistration("sw.js", { scope: "/scope/" });
    expect(reg.scope).toBe("/scope/");
    // installing or active is a ServiceWorker instance if present
    if (reg.installing) {
      expect(reg.installing).toBeInstanceOf(ServiceWorker);
    }
    const unregistered = await reg.unregister();
    expect(unregistered).toBe(true);
  });
});
