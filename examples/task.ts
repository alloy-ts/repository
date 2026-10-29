import { TaskThread } from "../dist/index.js";

async function main() {
  console.log("Creating worker task thread...");

  const task = new TaskThread("task.js");
  console.log(`Task created with threadId: ${task.threadId}`);

  task.addEventListener("message", "listener-1");

  task.postMessage("Hello from main thread!");

  const matched = task.dispatchEvent("message", "Dispatched synthetic message");
  console.log(`Dispatched synthetic message matched: ${matched}`);

  task.removeEventListener("message");

  task.terminate();
  console.log("Task terminated successfully.");
}

main().catch(console.error);
