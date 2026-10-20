// A cast of an external value reports S-14 (more specific than S-02).
import type { Settings } from "../../_shared/models";

const s = JSON.parse("{}") as Settings; // @expect TSR1401
export const theme: string = s.theme;
