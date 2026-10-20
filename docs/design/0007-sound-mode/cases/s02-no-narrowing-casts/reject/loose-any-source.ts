// A value from a loose package is any; for S-02 an any source counts as unknown.
import { config } from "loose-pkg";

interface Port {
  readonly n: number;
}
const port = config as Port; // @expect TSR0201
export const n: number = port.n;
