// {} to a type with required properties.
interface Config {
  readonly port: number;
  readonly host: string;
}
const cfg = {} as Config; // @expect TSR0201
export const port: number = cfg.port;
