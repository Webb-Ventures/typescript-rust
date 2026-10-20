// A union to one of its members.
import type { Circle, Shape } from "../../_shared/models";

function radius(shape: Shape): number {
  const c = shape as Circle; // @expect TSR0201
  return c.radius;
}
export const r: number = radius({ kind: "square", side: 2 });
