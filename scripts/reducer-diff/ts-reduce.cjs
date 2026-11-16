// The TypeScript reducer over stdin, with ids and the clock pinned as in crates/core/examples/reduce_lines.rs.
//   node ts-reduce.cjs <ai-coding-tool/dist>
const dist = process.argv[2];
const readline = require("node:readline");
let fixedNow = 0;
let ids = 0;
Date.now = () => fixedNow;
const pad = (n) => String(n).padStart(12, "0");
globalThis.crypto.randomUUID = () => `00000000-0000-4000-8000-${pad(ids++)}`;
const { reduce } = require(`${dist}/main/application/workspace-reducer.js`);
const { emptyWorkspaceState } = require(`${dist}/main/application/workspace-state.js`);
let state = emptyWorkspaceState();
const plain = (key, value) => value instanceof Set ? [...value] : value instanceof Map ? Object.fromEntries(value) : value;
const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line) => {
  let asked;
  try { asked = JSON.parse(line); } catch { console.log(JSON.stringify({ error: "unreadable line" })); return; }
  if (asked.reset) { state = emptyWorkspaceState(); ids = 0; console.log(JSON.stringify({ state }, plain)); return; }
  fixedNow = asked.now ?? 0;
  try {
    const transition = reduce(state, asked.input);
    state = transition.state;
    console.log(JSON.stringify({ state, effects: transition.effects, result: transition.result, ids }, plain));
  } catch (error) {
    console.log(JSON.stringify({ error: `threw: ${error && error.stack || error}` }));
  }
});
