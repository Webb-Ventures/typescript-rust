// @filename: globals.d.ts
declare module "node:console" {
    namespace console {
        interface ConsoleOptions {
            stdout: string;
        }
        interface Console {
            readonly Console: {
                prototype: Console;
                new (options: ConsoleOptions): Console;
            };
            log(...data: any[]): void;
        }
    }
    var console: console.Console;
    export = console;
}
// @filename: web.d.ts
export {};
import * as console from "node:console";
declare global {
    interface Console extends console.Console {}
}
// @filename: use.ts
import { Console as Console_2 } from "node:console";
export declare class BufferedConsole extends Console_2 {
    Console: typeof Console_2;
}
const c = new BufferedConsole();
c.log(1);
