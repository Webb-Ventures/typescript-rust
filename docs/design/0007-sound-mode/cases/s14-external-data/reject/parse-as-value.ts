// parse used as a value can't be expanded.
import { parse } from "@ts-r.dev/guard";

const p = parse; // @expect TSR1503
export const v: unknown = p(1);
