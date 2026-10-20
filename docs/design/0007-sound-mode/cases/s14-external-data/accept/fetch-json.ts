// fetchJson with an explicit type argument.
import { fetchJson } from "@ts-r.dev/guard";
import type { User } from "../../_shared/models";

const user: User = await fetchJson<User>("/api/user");
export const name: string = user.name;
