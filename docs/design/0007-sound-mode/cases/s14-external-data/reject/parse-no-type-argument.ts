// parse needs an explicit type argument.
import { parse } from "@ts-r.dev/guard";

const v = parse(JSON.parse("{}")); // @expect TSR1402
export const got: unknown = v;
