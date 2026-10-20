// A method overloaded across two global augmentations is instantiated with its interface.
// @filename: b.d.ts
export {};
declare global {
  interface Sub<T> {
    next<Value extends T>(value: Value): void;
    other(value: T): void;
  }
}
// @filename: c.d.ts
export {};
declare global {
  interface Sub<T> {
    next<Value extends T>(value: Value): void;
  }
}
// @filename: a.ts
import './b.js';
import './c.js';
declare const s: Sub<'first' | 'second'>;
s.next('first');
s.next('third');
const n: number = s.next;
