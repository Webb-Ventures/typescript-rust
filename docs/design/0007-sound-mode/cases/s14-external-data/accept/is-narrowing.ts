// is<T> narrows an external value.
import { is } from "@ts-r.dev/guard";
import type { User } from "../../_shared/models";

const data: unknown = JSON.parse('{"id":1}');
export const label: string = is<User>(data) ? data.name : "anonymous";
