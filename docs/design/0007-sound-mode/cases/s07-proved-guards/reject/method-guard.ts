// A method predicate whose true side includes squares.
import type { Circle, Shape } from "../../_shared/models";

class Shapes {
  isCircle(s: Shape): s is Circle { // @expect TSR0701
    return s.kind !== "square" || s.side > 0;
  }
}
function radius(s: Shape): number {
  return new Shapes().isCircle(s) ? s.radius : 0;
}
export const r: number = radius({ kind: "square", side: 1 });
