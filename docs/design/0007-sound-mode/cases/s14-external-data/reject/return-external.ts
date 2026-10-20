// Returning an external value as a declared type.
import type { Settings } from "../../_shared/models";

function read(text: string): Settings {
  return JSON.parse(text); // @expect TSR1401
}
export const s: Settings = read("{}");
