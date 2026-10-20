import { ParseError } from "@ts-r.dev/guard";

/** One line per outcome: the value, or the error's shape (record 0007, "Error shape"). */
export function report(label: string, run: () => unknown): void {
  try {
    const v = run();
    console.log(`${label}: ok ${JSON.stringify(v)}`);
  } catch (e) {
    if (e instanceof ParseError) {
      console.log(
        `${label}: ${e.name} type=${e.type} issues=${JSON.stringify(e.issues)} message=${JSON.stringify(e.message)} isError=${e instanceof Error}`,
      );
    } else if (e instanceof Error) {
      console.log(`${label}: ${e.name} ${e.message}`);
    } else {
      console.log(`${label}: threw ${String(e)}`);
    }
  }
}
