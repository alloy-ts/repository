import { Repository } from "../dist/index.js";

// Example demonstrating new Repository() and head.decode()
const repo = new Repository();

console.log("HEAD tree ID:", repo.headTreeId());
console.log("HEAD ref name:", repo.headName());

const head = repo.headCommit();
console.log("Decoded head commit:", head.decode());
