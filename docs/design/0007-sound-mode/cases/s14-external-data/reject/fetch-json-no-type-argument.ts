// fetchJson needs an explicit type argument.
import { fetchJson } from "@ts-r.dev/guard";

const u = await fetchJson("/api/user"); // @expect TSR1402
export const got: unknown = u;
