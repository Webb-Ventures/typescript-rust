// Adding a property to a package's interface claims every Item has it, unchecked.
declare module "typed-pkg" { // @expect TSR1101
  interface Item {
    readonly discount?: number;
  }
}
export {};
