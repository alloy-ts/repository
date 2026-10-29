import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Task } from "../src/main.ts";

// Demonstrates the Threaded Task API:
//   - new Task(scriptPath, options?)
//   - myTask.postMessage([val1, val2])
//   - myTask.onmessage = (e) => ...
//   - myTask.addEventListener("message", (e) => ...)
//   - myTask.terminate()

console.log("=== Threaded Task API Example ===");

// Create temporary worker script
const tmpDir = mkdtempSync(join(tmpdir(), "task-example-"));
const workerPath = join(tmpDir, "worker.js");
writeFileSync(
  workerPath,
  `
const { parentPort } = require('node:worker_threads');
if (parentPort) {
  parentPort.on('message', (data) => {
    parentPort.postMessage('Result: ' + JSON.stringify(data));
  });
}
`,
);

try {
  // Simulated input and result elements (as described in UI task specifications)
  const first = { value: "Hello", onchange: () => {} };
  const second = { value: "World", onchange: () => {} };
  const result = { textContent: "" };

  // Initialize task thread
  const myTask = new Task(workerPath, { name: "example-worker" });
  console.log(`Task created with threadId: ${myTask.threadId}`);

  myTask.onmessage = (e: any) => {
    result.textContent = e.data;
    console.log("Message received from task:", e.data);
  };

  myTask.addEventListener("message", (e: any) => {
    console.log("EventListener received message:", e.data);
  });

  [first, second].forEach((input) => {
    input.onchange = () => {
      myTask.postMessage([first.value, second.value]);
      console.log("Message posted to task");
    };
  });

  // Trigger change handlers
  first.onchange();
  second.onchange();

  // Dispatch synthetic event
  myTask.dispatchEvent({ type: "message", data: "Hello from synthetic event" });

  // Clean up
  myTask.terminate();
  console.log("Task example complete.");
} finally {
  rmSync(tmpDir, { recursive: true, force: true });
}
