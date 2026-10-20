// The refinement predicate sound.toml registers for Email ([sound.guard] refinements).
export type Email = string & { readonly __brand: "Email" };

// S-07 can't prove a brand: the predicate is what gives a string the brand, so it carries the hatch.
// @ts-r-unsafe S-07: a string matching the pattern is an Email by definition.
export function isEmail(v: unknown): v is Email {
  return typeof v === "string" && /^[^@\s]+@[^@\s]+$/.test(v);
}
