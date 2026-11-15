import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import { checkUpstream, upstream, UPSTREAM_SHA } from "../tests/server-parity/upstream.ts";

checkUpstream();
const ts = createRequire(resolve(upstream, "package.json"))("typescript");
const tests: { source: string; name: string; rustName: string }[] = [];
for (const component of ["core", "acl", "proxy", "static"]) {
  const source = `packages/server/${component}/tests/index.spec.ts`;
  const code = readFileSync(resolve(upstream, source), "utf8");
  const file = ts.createSourceFile(source, code, ts.ScriptTarget.Latest, true);
  function visit(node: any) {
    if (ts.isCallExpression(node)) {
      const title = node.arguments[0];
      const call = node.expression;
      let names: string[] = [];
      if (ts.isIdentifier(call) && call.text === "it" && title && ts.isStringLiteral(title)) {
        names = [title.text];
      } else if (ts.isCallExpression(call) && ts.isPropertyAccessExpression(call.expression)
        && call.expression.expression.getText(file) === "it" && call.expression.name.text === "each") {
        const cases = call.arguments[0];
        if (!title || !ts.isStringLiteral(title) || !cases || !ts.isArrayLiteralExpression(cases)) {
          throw new Error(`Unsupported it.each declaration in ${source}`);
        }
        names = cases.elements.map((value: any) => {
          if (!ts.isStringLiteral(value) || !title.text.includes("%s")) {
            throw new Error(`Unsupported it.each case in ${source}`);
          }
          return title.text.replace("%s", value.text);
        });
      }
      for (const name of names) {
        tests.push({ source, name, rustName: name.replace(/[^a-zA-Z0-9]+/g, "_").replace(/^_|_$/g, "").toLowerCase() });
      }
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
}
const output = JSON.stringify({ upstream: UPSTREAM_SHA, tests }, null, 2) + "\n";
const path = fileURLToPath(new URL("../tests/server-parity/inventory.json", import.meta.url));
if (process.argv.includes("--check")) {
  if (readFileSync(path, "utf8").replace(/\r\n/g, "\n") !== output) throw new Error("Server test inventory is stale; run bun run server:inventory");
} else {
  writeFileSync(path, output);
}
console.log(`${tests.length} upstream test cases inventoried (parameterized declarations expanded)`);
