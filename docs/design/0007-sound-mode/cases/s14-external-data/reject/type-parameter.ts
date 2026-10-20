// parse<T> inside generic code has no concrete type to validate.
import { parse } from "@ts-r.dev/guard";

function load<T>(raw: unknown): T {
  return parse<T>(raw); // @expect TSR1502
}
export const n: number = load<number>(1);
