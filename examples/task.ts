import { Task } from "../src/main.ts";

// Demonstrates the Threaded Tasks API bindings in src/task.rs:
//   - Task(script, options?)           (constructs a threaded task running in worker_threads)
//   - Task.postMessage(message, transfer?)  (sends structured-clone data to worker thread)
//   - Task.onmessage / Task.onerror    (handles events received from the worker)
//   - Task.addEventListener / remove   (registers event listeners)
//   - Task.terminate()                 (stops execution in the worker thread)

console.log("=== Threaded Task API Demo ===");

// Construct a new task running a worker script in a background thread.
const myTask = new Task("task.js", { name: "background-worker" });

console.log(`Task created with threadId: ${myTask.threadId}, script: ${myTask.script}`);

// Simulated DOM-like input elements for demonstration:
const first = { value: "10", onchange: null as null | (() => void) };
const second = { value: "20", onchange: null as null | (() => void) };
const result = { textContent: "" };

// Attach change handlers to inputs to post messages to the task thread:
[first, second].forEach((input) => {
  input.onchange = () => {
    myTask.postMessage([first.value, second.value]);
    console.log("Message posted to task:", [first.value, second.value]);
  };
});

// Handle incoming messages from the worker task:
myTask.onmessage = (e: any) => {
  result.textContent = String(e.data);
  console.log("Message received from task:", e.data);
};

// Listen for errors on the task thread:
myTask.onerror = (e: any) => {
  console.error("Task error:", e.message || e.error);
};

// Add an additional event listener using addEventListener:
const customListener = (e: any) => {
  console.log("Custom listener triggered with event:", e);
};
myTask.addEventListener("message", customListener);

// Simulate input changes triggering task processing:
if (first.onchange) first.onchange();
if (second.onchange) second.onchange();

// Dispatch a synthetic event to target listeners:
myTask.dispatchEvent({ type: "message", data: [30, 40] });

// Clean up and terminate the task thread when complete:
console.log("Terminating task worker...");
myTask.terminate();
console.log(`Task terminated status: ${myTask.terminated}`);

console.log("\nTask example complete.");
