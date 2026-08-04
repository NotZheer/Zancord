/**
 * Bounded in-memory log ring buffer for the debug log interceptor.
 * Prevents unbounded memory growth on long calls (PERF-AUDIT P4).
 */
export interface LogBuffer {
  push(line: string): void;
  /** Live view of the buffer contents, oldest first. */
  entries(): readonly string[];
}

export function createLogBuffer(cap = 500): LogBuffer {
  const buffer: string[] = [];
  return {
    push(line: string): void {
      buffer.push(line);
      if (buffer.length > cap) {
        buffer.splice(0, buffer.length - cap);
      }
    },
    entries(): readonly string[] {
      return buffer;
    },
  };
}
