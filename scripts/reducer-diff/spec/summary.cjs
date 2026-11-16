// Lists each distinct divergence spec.sh logged once: the test, the input, and where the answers differ.
const lines = require("node:fs").readFileSync(process.argv[2], "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
console.log(`${lines.length} divergences`);
const seen = new Set();
for (const line of lines) {
  const signature = line.file + "|" + line.problems.map((problem) => problem.replace(/\[\d+\]/g, "[]").replace(/ts=.*/, "")).join(",");
  if (seen.has(signature)) continue;
  seen.add(signature);
  console.log(`\n## ${line.file} :: ${line.test}\n  IN ${line.input.slice(0, 300)}`);
  for (const problem of line.problems.slice(0, 6)) console.log(`  - ${problem.slice(0, 500)}`);
}
