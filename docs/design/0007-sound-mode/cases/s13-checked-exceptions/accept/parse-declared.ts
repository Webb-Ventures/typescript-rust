// parse<T> declares ParseError, so the exported caller declares it too.
import { parse, type ParseError } from "@ts-r.dev/guard";
import type { Settings } from "../../_shared/models";

/** @throws {ParseError} */
export function readSettings(text: string): Settings {
  return parse<Settings>(JSON.parse(text));
}
