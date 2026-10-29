import { writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { Task, isMainThread, getThreadId } from "../src/main.ts";

// Create a worker script file for the example
const workerFile = join(process.cwd(), "example_worker.cjs");
writeFileSync(
  workerFile,
  `
  const { parentPort } = require('node:worker_threads');
  if (parentPort) {
    parentPort.on('message', (inputs) => {
      console.log("[Worker] Message received from main thread:", inputs);
      const result = inputs.reduce((a, b) => Number(a) + Number(b), 0);
      parentPort.postMessage("Result: " + result);
    });
  }
`,
  "utf8",
);

console.log(`=== (Threaded) Task Example ===`);
console.log(`isMainThread: ${isMainThread()}, main thread ID: ${getThreadId()}`);

// Instantiate the Task
const myTask = new Task(workerFile, { name: "example-worker" });
console.log(`Task created with threadId: ${myTask.threadId}, name: ${myTask.threadName}`);

// Handle messages received from the task thread
myTask.onmessage = (e: any) => {
  const result = { textContent: e.data };
  console.log("Message received from task:", result.textContent);
};

// Simulate inputs sending messages to the task
const first = { value: 10 };
const second = { value: 20 };

[first, second].forEach((input) => {
  const onchange = () => {
    myTask.postMessage([first.value, second.value]);
    console.log("Message posted to task");
  };
  onchange();
});

// Clean up after 500ms
setTimeout(() => {
  myTask.terminate();
  try {
    unlinkSync(workerFile);
  } catch {}
  console.log("Task terminated and cleanup complete.");
}, 500);
