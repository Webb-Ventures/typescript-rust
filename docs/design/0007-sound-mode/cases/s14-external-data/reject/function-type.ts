// A function type has no runtime shape to check.
import { parse } from "@ts-r.dev/guard";

const raw: unknown = null;
const f = parse<(x: number) => string>(raw); // @expect TSR1501
export const out: string = f(1);
