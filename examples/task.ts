import { writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { Task } from "../src/main.ts";

// Demonstrates the Threaded Tasks API (src/task.rs):
//   - Task(scriptPath, options?)
//   - task.postMessage(message, transfer?)
//   - task.onmessage = (e) => ...
//   - task.ref() / task.unref()
//   - task.terminate()

const workerScript = join(process.cwd(), "example-worker.js");
writeFileSync(
  workerScript,
  `
const { parentPort } = require('node:worker_threads');
parentPort.on('message', (data) => {
  console.log("Task thread received message:", data);
  if (Array.isArray(data)) {
    const result = Number(data[0]) + Number(data[1]);
    parentPort.postMessage("Result: " + result);
  }
});
`
);

async function main() {
  console.log("=== Threaded Tasks Example ===");

  const myTask = new Task(workerScript, { name: "calculator-task" });
  console.log(`Task created with thread ID: ${myTask.threadId}`);

  myTask.onmessage = (e: any) => {
    console.log("Message received from task:", e.data);
  };

  // Mock inputs reflecting the user prompt pattern
  const first = { value: "10" };
  const second = { value: "20" };

  [first, second].forEach((input) => {
    myTask.postMessage([input.value, second.value]);
    console.log("Message posted to task");
  });

  // Give the worker thread time to process messages
  await new Promise((resolve) => setTimeout(resolve, 300));

  await myTask.terminate();
  console.log("Task terminated.");

  try {
    unlinkSync(workerScript);
  } catch {}
}

main().catch(console.error);
