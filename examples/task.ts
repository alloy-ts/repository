import { Task } from "../src/main.ts";
import { writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

// Create a worker script file "task.js" that receives messages and responds.
const taskScriptPath = join(tmpdir(), `task_demo_${Date.now()}.js`);
writeFileSync(
  taskScriptPath,
  `
  const { parentPort } = require("node:worker_threads");
  if (parentPort) {
    parentPort.on("message", (data) => {
      const [first, second] = data;
      const sum = Number(first) + Number(second);
      parentPort.postMessage("Sum is " + sum);
    });
  }
  `
);

async function main() {
  console.log("=== Threaded Task API Example ===");

  const myTask = new Task(taskScriptPath);
  console.log(`Task created with threadId: ${myTask.threadId}`);

  // Mock UI input element objects for demonstration
  const first = { value: "10", onchange: () => {} };
  const second = { value: "20", onchange: () => {} };
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

  // Simulate user changing inputs
  console.log("\nSimulating user input change: first=10, second=20...");
  first.onchange();

  // Wait for worker response
  await new Promise((resolve) => setTimeout(resolve, 500));

  console.log("Result textContent:", result.textContent);

  // Simulate another change
  console.log("\nSimulating user input change: first=35, second=65...");
  first.value = "35";
  second.value = "65";
  second.onchange();

  await new Promise((resolve) => setTimeout(resolve, 500));
  console.log("Result textContent:", result.textContent);

  // Clean up
  await myTask.terminate();
  try {
    unlinkSync(taskScriptPath);
  } catch {}

  console.log("\nTask example complete.");
}

main().catch(console.error);
