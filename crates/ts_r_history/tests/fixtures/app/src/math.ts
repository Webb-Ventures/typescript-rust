export interface Point {
  x: number;
  y: number;
}

export function add(a: Point, b: Point): Point {
  return { x: a.x + b.x, y: a.y + b.y };
}

export function scale(p: Point, k: number): Point {
  return { x: p.x * k, y: p.y * k };
}

export const ORIGIN: Point = { x: 0, y: 0 };

export function sum(values: number[]): number {
  return values.reduce((a, b) => a + b, 0);
}

export function pick<T, K extends keyof T>(obj: T, keys: K[]): Pick<T, K> {
  const out = {} as Pick<T, K>;
  for (const k of keys) {
    out[k] = obj[k];
  }
  return out;
}
