// A non-sound local file's any arrives as unknown and is parsed.
import { parse } from "@ts-r.dev/guard";
import { legacyConfig } from "../../_shared/legacy";
import type { Settings } from "../../_shared/models";

const settings: Settings = parse<Settings>(legacyConfig);
export const size: number = settings.fontSize;
