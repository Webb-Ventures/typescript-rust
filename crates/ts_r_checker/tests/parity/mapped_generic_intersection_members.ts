// A generic `{ [K in keyof X]-?: X[K] }` over a generic intersection X has the properties of
// X's members' constraints (@eslint/core's CustomRuleDefinitionType).
export {};
interface CTD { RuleOptions: unknown[]; MessageIds: string; ExtRuleDocs: Record<string, unknown> }
interface Opts { LangOptions: object; Code: object; RuleOptions: unknown[]; MessageIds: string; ExtRuleDocs: Record<string, unknown> }
type RD<O extends Opts = Opts> = { o: O };
type RE<O extends CTD = CTD> = { o: O };
type C3<O extends Partial<CTD>> = RE<Required<O & Omit<CTD, keyof O>>>;
type C4<L extends Omit<Opts, keyof CTD>> = RD<L & CTD>;
function f<L extends Omit<Opts, keyof CTD>, O extends Partial<CTD>>(y: L & Required<O & Omit<CTD, keyof O>>) {
  const b: Opts = y;
}
