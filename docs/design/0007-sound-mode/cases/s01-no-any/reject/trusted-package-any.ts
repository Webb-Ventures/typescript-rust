// typed-pkg declares legacyLookup(): any; in a sound file the result is unknown.
import { legacyLookup } from "typed-pkg";

const label: string = legacyLookup("a1"); // @expect TSR0102
export { label };
