// ts-r-run preload (strategy "hook"): off-thread module.register hooks that ask a
// long-lived `ts-r-run --serve` process to resolve and compile each module on demand.
import { register, setSourceMapsSupport } from 'node:module';

const env = process.env;
const data = { bin: env.TS_R_RUN_BIN, tsconfig: env.TS_R_RUN_TSCONFIG ?? '', entry: env.TS_R_RUN_ENTRY ?? '', sourceMaps: env.TS_R_RUN_SOURCE_MAPS !== '0' };
const sourceMaps = data.sourceMaps;
for (const k of ['TS_R_RUN_BIN', 'TS_R_RUN_TSCONFIG', 'TS_R_RUN_ENTRY', 'TS_R_RUN_SOURCE_MAPS']) delete env[k];
if (sourceMaps) setSourceMapsSupport(true);
register(new URL('./hooks-async.mjs', import.meta.url), { data });
