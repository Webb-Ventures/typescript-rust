// {} is assignable to a type whose properties are all optional, so this is an upcast.
interface Options {
  readonly retries?: number;
  readonly verbose?: boolean;
}
const defaults = {} as Options;
export const retries: number = defaults.retries ?? 3;
