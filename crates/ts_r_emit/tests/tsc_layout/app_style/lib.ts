// No `target`: numeric literals with separators print by value.
export const TIMEOUT_MS = 10_000;
export const MASK = 0xFF_FF;
export const PLAIN = 0xff;
export type Options = { retries: number };
export interface Logger {
  log(message: string): void;
}
export const facets = (name: string, values: string[]) => values.map((v) => `${name}:${v}`);
