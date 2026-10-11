// The stylesheet sources joined in name order, as `stylesheet()` in
// `site/export.vyrn` joins them.
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

const dir = fileURLToPath(new URL("../public/css/", import.meta.url));

export const sheet = readdirSync(dir).sort().map((name) => readFileSync(dir + name, "utf8")).join("");
