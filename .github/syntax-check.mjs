import { readdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
for(const dir of ['.github','.github/tests']) for(const file of readdirSync(dir)) if(file.endsWith('.mjs')) execFileSync(process.execPath,['--check',`${dir}/${file}`],{stdio:'inherit'});
