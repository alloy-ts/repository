# Engine API Documentation

The `engine` module implements a high-performance bytecode compiler and VM runtime for the custom Engine programming language, integrated with FuseVM and Node-API bindings.

## Overview

The engine provides:

- **Lexer & Parser**: Tokenization and AST parsing for expressions, control flow, functions, and symbols.
- **Bytecode Compiler**: Translates Engine AST into compact bytecode instructions executable by FuseVM.
- **VM Execution**: Fast in-process virtual machine execution with JIT support and host environment binding.
- **NAPI-RS Classes**: `Engine` and `Program` exported directly to Node.js / TypeScript.

---

## Classes

### `Engine`

The main execution engine and VM context for compiling and running Engine source code.

#### Constructor

```ts
new Engine();
```

#### Methods

- `compile(source: string): Program`: Parses and compiles Engine source code into an executable `Program` bytecode container.
- `eval(source: string): any`: Evaluates source code immediately and returns the resulting value.
- `evalAsync(source: string): Promise<any>`: Asynchronously evaluates Engine source on the thread pool without blocking the main event loop.

### `Program`

A compiled bytecode container ready for repeated execution.

#### Methods

- `run(): any`: Executes the compiled bytecode in the VM.
- `runAsync(): Promise<any>`: Runs the compiled bytecode asynchronously on the thread pool.
- `disassemble(): string`: Returns the human-readable disassembled bytecode instructions.

---

## Example Usage

```ts
import { Engine } from "@alloy-ts/repository";

const engine = new Engine();

// Synchronous evaluation
const result = engine.eval("let x = 10 + 20; x * 2;");
console.log(result); // 60

// Compile once, run multiple times
const program = engine.compile("fn square(n) { n * n }; square(8);");
console.log(program.run()); // 64
console.log(await program.runAsync()); // 64
```
