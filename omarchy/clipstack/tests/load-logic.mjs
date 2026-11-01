// Loads Logic.js (a QML `.pragma library` script) into a plain vm context so
// node can test it without Qt. The file must stay plain functions.
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

export function loadLogic() {
  const file = path.join(here, "..", "Logic.js");
  const code = fs.readFileSync(file, "utf8").replace(/^\.pragma library\s*$/m, "");
  const context = vm.createContext({});
  vm.runInContext(code, context, { filename: file });
  return context;
}
