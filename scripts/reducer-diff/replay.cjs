// Replays a trail up to a step through both reducers and prints an expression of each state before and after it:
//   node replay.cjs <dist> <rust-binary> <trail> <step> '<expr of s (state) and o (answer)>' 
const { spawnSync } = require("child_process");
const fs = require("fs");
const [dist, rustBin, trail, step, expr] = process.argv.slice(2);
const lines = fs.readFileSync(trail, "utf8").trim().split("\n").slice(0, Number(step) + 1);
const input = ['{"reset":true}', ...lines].join("\n") + "\n";
const run = (cmd, args) => spawnSync(cmd, args, { input, maxBuffer: 1 << 30 }).stdout.toString().trim().split("\n");
const ts = run("node", [__dirname + "/ts-reduce.cjs", dist]);
const rs = run(rustBin, []);
const f = new Function("s", "o", `return (${expr});`);
for (const [name, out] of [["ts", ts], ["rust", rs]]) {
  const before = JSON.parse(out[out.length - 2]), after = JSON.parse(out[out.length - 1]);
  console.log(name, "before:", JSON.stringify(f(before.state, before)), "\n   after:", JSON.stringify(f(after.state, after)));
}
