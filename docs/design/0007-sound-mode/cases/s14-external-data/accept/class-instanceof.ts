// A class is checked with instanceof against the class itself, which the validator imports.
import { parse } from "@ts-r.dev/guard";

export class Point {
  readonly x: number;
  readonly y: number;
  constructor(x: number, y: number) {
    this.x = x;
    this.y = y;
  }
}
const raw: unknown = new Point(1, 2);
export const p: Point = parse<Point>(raw);
