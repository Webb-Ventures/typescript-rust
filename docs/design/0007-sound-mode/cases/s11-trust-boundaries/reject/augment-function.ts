// Declaring an extra export of a package claims it exists, unchecked.
declare module "verify-pkg" { // @expect TSR1101
  export function extra(): string;
}
export {};
