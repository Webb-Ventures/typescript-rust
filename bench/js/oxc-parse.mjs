// Parse one file with oxc-parser (reference for the parse-throughput scenario).
// Usage: node bench/js/oxc-parse.mjs <file>
import { readFileSync } from "node:fs";
import { parseSync } from "oxc-parser";

const file = process.argv[2];
const source = readFileSync(file, "utf8");
const result = parseSync(file, source);
if (result.errors.length > 0) {
  console.error(`${result.errors.length} parse errors in ${file}`);
}
