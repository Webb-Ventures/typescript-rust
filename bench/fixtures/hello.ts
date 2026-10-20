// Start-up fixture for the `run-startup` scenario: tiny, typed, one import.
import { basename } from "node:path";

interface Greeting {
  name: string;
  count: number;
}

const greeting: Greeting = { name: basename("/ts-r"), count: 1 };
console.log(`hello from ${greeting.name} x${greeting.count}`);
