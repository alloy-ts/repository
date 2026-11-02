# Scheduler API

The `scheduler` module provides cooperative and preemptive task scheduling, prioritization, and execution queues for asynchronous tasks and worker threads.

## Overview

The scheduler manages workload distribution across available worker threads and threadpools, allowing:
- Priority queue management.
- Task cancellation and deadline scheduling.
- Worker thread task distribution.

## API Reference

### Functions and Classes

- `Scheduler`: Core task scheduler managing task queues, priorities, and worker allocation.
- Priority levels: Low, Normal, High, Critical.
