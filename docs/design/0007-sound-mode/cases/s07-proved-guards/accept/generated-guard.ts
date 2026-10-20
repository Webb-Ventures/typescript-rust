// A guard that delegates to a generated is<T>.
import { is } from "@ts-r.dev/guard";
import type { User } from "../../_shared/models";

const isUser = (x: unknown): x is User => is<User>(x);
const v: unknown = { id: 1, name: "a", roles: [] };
export const n: string = isUser(v) ? v.name : "";
