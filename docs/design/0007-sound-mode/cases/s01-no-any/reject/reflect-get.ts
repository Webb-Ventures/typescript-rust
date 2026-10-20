// Reflect.get returns any in lib.
function port(config: object): number {
  const p: number = Reflect.get(config, "port"); // @expect TSR0102
  return p;
}
export const p: number = port({ port: 80 });
