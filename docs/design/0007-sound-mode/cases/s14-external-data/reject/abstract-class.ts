// An abstract class: validate one of its concrete subclasses instead.
import { parse } from "@ts-r.dev/guard";

export abstract class Shape {
  abstract area(): number;
}
const raw: unknown = null;
export const s: Shape = parse<Shape>(raw); // @expect TSR1501
