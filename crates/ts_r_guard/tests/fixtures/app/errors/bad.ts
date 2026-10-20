// Call sites the transform rejects, with the codes ts-r-check reports for them.
import { parse, is } from "@ts-r.dev/guard";
import type { User } from "../types";

export const fn = (x: unknown) => parse<() => void>(x); // TSR1501
export const noType = (x: unknown) => parse(x); // TSR1402
export const asValue = [1].map(is); // TSR1503
export const ok = (x: unknown) => parse<User>(x);
