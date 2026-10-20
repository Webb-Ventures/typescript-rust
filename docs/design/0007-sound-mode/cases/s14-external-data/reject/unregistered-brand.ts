// A branded type without a registered refinement: nothing at runtime tells a UserId from a string.
import { parse } from "@ts-r.dev/guard";

export type UserId = string & { readonly __brand: "UserId" };
const raw: unknown = "u1";
export const id: UserId = parse<UserId>(raw); // @expect TSR1501
