// A class imported with 'import type' has no value at the call site to check instanceof against.
import { parse } from "@ts-r.dev/guard";
import type { Point } from "../accept/class-instanceof";

const raw: unknown = null;
export const p: Point = parse<Point>(raw); // @expect TSR1501
