// unknown to a specific type.
import type { User } from "../../_shared/models";

function load(data: unknown): User {
  return data as User; // @expect TSR0201
}
export const u: User = load({ id: 1, name: "a", roles: [] });
