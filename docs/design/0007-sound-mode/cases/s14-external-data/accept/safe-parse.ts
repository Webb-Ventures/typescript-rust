// safeParse returns a Result instead of throwing.
import { safeParse } from "@ts-r.dev/guard";
import type { Settings } from "../../_shared/models";

const r = safeParse<Settings>(JSON.parse("{}"));
export const size: number = r.ok ? r.value.fontSize : 12;
