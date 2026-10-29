import { writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Task } from "../src/main.ts";

// Create a temporary task script for demonstration
const taskScript = `
  const { parentPort } = require('node:worker_threads');
  if (parentPort) {
    parentPort.on('message', (data) => {
      if (Array.isArray(data)) {
        const [a, b] = data;
        const result = String(Number(a || 0) + Number(b || 0));
        parentPort.postMessage(result);
      }
    }
  );
}`;

const scriptPath = join(tmpdir(), "example_task.js");
writeFileSync(scriptPath, taskScript);

console.log("Starting task from:", scriptPath);
const myTask = new Task(scriptPath, { name: "calculator-task" });

console.log("Task created with threadId:", myTask.threadId);

const first = { value: "10", onchange: null as any };
const second = { value: "32", onchange: null as any };
const result = { textContent: "" };

[first, second].forEach((input) => {
  input.onchange = () => {
    myTask.postMessage([first.value, second.value]);
    console.log("Message posted to task");
  };
});

myTask.onmessage = (e: any) => {
  result.textContent = e.data;
  console.log("Message received from task:", e.data);
};

// Simulate input change
first.onchange();

// Clean up demonstration
setTimeout(async () => {
  await myTask.terminate();
  try {
    rmSync(scriptPath, { force: true });
  } catch {}
  console.log("Task demo complete.");
}, 500);
