import { writeFileSync, unlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Task } from "../src/main.ts";

// Create a worker script file `task.js` that performs background computation (e.g. multiplying two numbers)
const workerScriptPath = join(tmpdir(), "task-worker-example.js");
writeFileSync(
  workerScriptPath,
  `
  const { parentPort } = require('node:worker_threads');
  parentPort.on('message', (data) => {
    const [a, b] = data;
    const result = Number(a) * Number(b);
    parentPort.postMessage(result);
  });
  `,
);

try {
  console.log("=== Threaded Tasks API Example ===");

  // Instantiate the Task
  const myTask = new Task(workerScriptPath, { name: "multiplier-task" });
  console.log(`Task created with threadId: ${myTask.threadId}`);

  // Mock DOM-like elements for demonstration
  const first = { value: "6", onchange: () => {} };
  const second = { value: "7", onchange: () => {} };
  const result = { textContent: "" };

  // Set up message event handler
  myTask.onmessage = (e: any) => {
    result.textContent = String(e.data);
    console.log("Message received from task:", e.data);
  };

  // Wire up change listeners on inputs
  [first, second].forEach((input) => {
    input.onchange = () => {
      myTask.postMessage([first.value, second.value]);
      console.log("Message posted to task:", [first.value, second.value]);
    };
  });

  // Trigger input change
  first.onchange();

  // Wait briefly for worker response
  await new Promise((resolve) => setTimeout(resolve, 300));
  console.log("Result textContent:", result.textContent);

  // Demonstrate event listener API
  myTask.addEventListener("message", (e: any) => {
    console.log("EventListener received message:", e.data);
  });

  second.value = "9";
  second.onchange();

  await new Promise((resolve) => setTimeout(resolve, 300));
  console.log("Result textContent after update:", result.textContent);

  // Clean up
  await myTask.terminate();
  console.log(`Task terminated (terminated = ${myTask.terminated})`);
} finally {
  try {
    unlinkSync(workerScriptPath);
  } catch {}
}
