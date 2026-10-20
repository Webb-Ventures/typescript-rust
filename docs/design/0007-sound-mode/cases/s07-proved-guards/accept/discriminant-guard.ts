// A discriminant check proves the predicate.
import type { Circle, Shape } from "../../_shared/models";

function isCircle(s: Shape): s is Circle {
  return s.kind === "circle";
}
function size(s: Shape): number {
  return isCircle(s) ? s.radius : s.side;
}
export const r: number = size({ kind: "circle", radius: 1 });
