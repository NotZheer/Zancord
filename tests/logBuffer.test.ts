import { describe, it, expect } from 'vitest';
import { createLogBuffer } from '../src/utils/logBuffer';

describe('createLogBuffer (P4: bounded debug log memory)', () => {
  it('keeps at most cap entries', () => {
    const buf = createLogBuffer(3);
    buf.push('a');
    buf.push('b');
    buf.push('c');
    buf.push('d');
    expect(buf.entries()).toEqual(['b', 'c', 'd']);
  });

  it('drops the oldest entries first', () => {
    const buf = createLogBuffer(2);
    for (let i = 0; i < 10; i++) buf.push(`line-${i}`);
    expect(buf.entries()).toEqual(['line-8', 'line-9']);
  });

  it('defaults to a 500-entry cap', () => {
    const buf = createLogBuffer();
    for (let i = 0; i < 600; i++) buf.push(`line-${i}`);
    expect(buf.entries().length).toBe(500);
    expect(buf.entries()[0]).toBe('line-100');
  });

  it('returns a live view of the buffer', () => {
    const buf = createLogBuffer(2);
    const view = buf.entries();
    expect(view.length).toBe(0);
    buf.push('x');
    expect(view).toEqual(['x']);
  });
});
