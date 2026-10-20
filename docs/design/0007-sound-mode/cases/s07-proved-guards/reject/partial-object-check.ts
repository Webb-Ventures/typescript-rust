// Checking for an object with an id doesn't prove it is a User.
import type { User } from "../../_shared/models";

function isUser(x: unknown): x is User { // @expect TSR0701
  return typeof x === "object" && x !== null && "id" in x;
}
const v: unknown = { id: 1 };
export const n: string = isUser(v) ? v.name : "";
