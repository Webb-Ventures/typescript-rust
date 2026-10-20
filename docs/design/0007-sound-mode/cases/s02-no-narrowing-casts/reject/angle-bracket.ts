// The angle-bracket form is the same assertion.
import type { User } from "../../_shared/models";

function load(data: unknown): User {
  return <User>data; // @expect TSR0201
}
export const u: User = load({});
