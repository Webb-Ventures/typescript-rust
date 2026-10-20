// A non-sound local file is treated like a trusted package: its any is unknown here.
import { legacyCount } from "../../_shared/legacy";

const n: number = legacyCount("abc"); // @expect TSR0102
export { n };
