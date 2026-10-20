// JSON.parse result parsed against a declared type.
import { parse } from "@ts-r.dev/guard";
import type { Settings } from "../../_shared/models";

const text = '{"theme":"dark","fontSize":14}';
const settings: Settings = parse<Settings>(JSON.parse(text));
export const size: number = settings.fontSize;
