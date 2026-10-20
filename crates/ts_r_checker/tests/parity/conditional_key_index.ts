// date-fns `buildLocalizeFn` (`valuesArray[index]` with `index: LocalizeUnitIndex<Value>`):
// a conditional key whose `number` branch is not a key of the conditional object is TS2536.
// `keyof` a union of tuples and a record is the intersection of their key sets (`never`),
// computed as sets rather than as a cross product.
type Era = 0 | 1;
type Quarter = 1 | 2 | 3 | 4;
type DayPeriod = "am" | "pm";
type Day = 0 | 1 | 2 | 3 | 4 | 5 | 6;
type Month = 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11;
type UnitValue = Era | Quarter | Month | Day | DayPeriod;
type UnitIndex<Value extends UnitValue | number> = Value extends UnitValue ? keyof Values<Value> : number;
type Values<Value extends UnitValue> = Value extends DayPeriod
  ? Record<DayPeriod, string>
  : Value extends Era
    ? readonly [string, string]
    : Value extends Quarter
      ? readonly [string, string, string, string]
      : Value extends Day
        ? readonly [string, string, string, string, string, string, string]
        : Value extends Month
          ? readonly [string, string, string, string, string, string, string, string, string, string, string, string]
          : never;
function f<Value extends UnitValue>(values: Values<Value>, index: UnitIndex<Value>): string {
  return values[index];
}
function h<V extends UnitValue>(k: keyof Values<V>) {
  const m: boolean = k;
  const z: keyof Values<V> = 5 as number;
}
export {};
