// A branded type and its refinement predicate, registered in tsrconfig.json (record 0007,
// open question 14).
export type Email = string & { readonly __brand: "Email" };

export const isEmail = (v: unknown): v is Email =>
  typeof v === "string" && /^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(v);

// A refined number: string inputs (`query`, `env`, `args`) convert by the base type first.
export type Port = number & { readonly __brand: "Port" };

export const isPort = (v: unknown): v is Port =>
  typeof v === "number" && Number.isInteger(v) && v > 0 && v < 65536;

// A refinement of a plain alias: its validator imports the base declaration's.
export type Base = { x: number };
export type Positive = Base;

export const isPositive = (v: unknown): v is Positive =>
  typeof v === "object" && v !== null && (v as Base).x > 0;
