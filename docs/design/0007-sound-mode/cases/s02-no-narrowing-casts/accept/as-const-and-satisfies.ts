// as const and satisfies never narrow.
interface Config {
  readonly port: number;
  readonly host: string;
}
const directions = ["up", "down"] as const;
const partial = { port: 80 } satisfies Partial<Config>;
export const first: "up" = directions[0];
export const port: number = partial.port;
