export const a: number = 'not a number';

const o = { a: 1 };
export const b = o.missing;

export function f(x: string, n: number): void {
  f(n, x);
}
