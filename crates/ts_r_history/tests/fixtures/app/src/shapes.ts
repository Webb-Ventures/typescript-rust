import { add, ORIGIN, type Point, sum } from "./math";

export interface Box {
  min: Point;
  max: Point;
}

export type Shape =
  | { kind: "circle"; center: Point; radius: number }
  | { kind: "rect"; box: Box };

export function area(s: Shape): number {
  switch (s.kind) {
    case "circle":
      return Math.PI * s.radius ** 2;
    case "rect":
      return (s.box.max.x - s.box.min.x) * (s.box.max.y - s.box.min.y);
  }
}

export function totalArea(shapes: Shape[]): number {
  return sum(shapes.map(area));
}

export function moved(b: Box, by: Point): Box {
  return { min: add(b.min, by), max: add(b.max, by) };
}

export class Canvas {
  private shapes: Shape[] = [];
  add(s: Shape): this {
    this.shapes.push(s);
    return this;
  }
  get count(): number {
    return this.shapes.length;
  }
  center(): Point {
    return ORIGIN;
  }
}
