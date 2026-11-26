const {appendFileSync, writeFileSync} = require('node:fs');
const {join} = require('node:path');

const directory = process.env.RUNNER_TEMP;
const output = process.env.GITHUB_ENV;
if (!directory || !output) {
  throw new Error('The Go setup action requires runner temporary and environment paths');
}
const file = join(directory, 'dalgona-go.env').replaceAll('\\', '/');
writeFileSync(file, 'GOCACHEPROG=\n', {mode: 0o600});
delete process.env.GOCACHEPROG;
process.env.GOENV = file;
appendFileSync(output, `GOENV=${file}\n`);
