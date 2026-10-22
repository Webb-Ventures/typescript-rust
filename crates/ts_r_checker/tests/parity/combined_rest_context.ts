// Overloads combine into one contextual signature also when they have rest parameters, and
// an immediately invoked function's rest parameter takes a spread iterable's elements.
interface Logger {
    log(...data: any[]): void;
    log(message?: any, ...optionalParams: any[]): void;
}
declare const logger: Logger;
const logs: string[] = [];
logger.log = (...args) => {
    logs.push(...args);
};
export const test2: ((a: number, ...args: []) => void) &
    ((b: string) => void) &
    ((c: boolean) => void) = (arg) => {};
declare const itNum: Iterable<number>;
(function (...rest) { const n: number[] = rest; })(...itNum);
(function (a, ...rest) { const n: number[] = rest; })("", ...itNum);
