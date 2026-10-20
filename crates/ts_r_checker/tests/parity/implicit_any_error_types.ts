// Types from an unresolved import are tsc's error type (`any`): callbacks reached through
// them still get TS7006 (property access through a constraint, tagged templates,
// `Awaited`, `Promise.all` tuples, element access).
import { sql, type Executor, z } from 'missing-module'
export async function f<S extends z.ZodObject>(db: Executor, schema: S) {
  schema.safeParse(1).error.issues.map((i) => i)
  const { rows } = await sql<{ a: number }>`select ${db}`.execute(db)
  rows.map((r) => r)
  const [a3, b3] = await Promise.all([db.a(), db.b()])
  a3.map((x3) => x3)
  b3.map((y3) => y3)
  const all = await Promise.all([db, db])
  all[0].map((x4) => x4)
}
declare const aw: Awaited<Executor>
aw.map((x1) => x1)
