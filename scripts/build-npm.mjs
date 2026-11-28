// npm に出すパッケージ（dist/）を作る。web/ の TS を JS と型定義（.d.ts）にし、wasm とグルーを写す。
// web/ は wevocal-lib へ写す正本（TS のまま）で、npm では TS を読めない環境のために JS にする
// 使い方: ./build-wasm.sh と node scripts/export-to-lib.mjs のあとに node scripts/build-npm.mjs
import { execSync } from 'node:child_process'
import { copyFileSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const web = join(root, 'web')
const out = join(root, 'dist')
rmSync(out, { recursive: true, force: true })
const files = readdirSync(web).filter((f) => f.endsWith('.ts') && !f.endsWith('.d.ts'))
// 型は確かめずに書き出す（Worker のファイルは webworker の型、ほかは DOM の型で、1 つのプログラムでは両方を読めないため。型の確かめは wevocal-lib の側で行う）
execSync(
  `npx -y -p typescript@6 tsc --noCheck --declaration --target es2022 --module esnext --moduleResolution bundler --lib es2022,dom --skipLibCheck --outDir dist ${files.map((f) => `web/${f}`).join(' ')}`,
  { cwd: root, stdio: 'inherit' },
)
// Worker の場所はビルドしたあとの .js を指し、拡張子の無い相対の import には .js を付ける（バンドラーを通さないブラウザや Node の ESM でも読めるように）
for (const f of readdirSync(out).filter((f) => f.endsWith('.js') || f.endsWith('.d.ts'))) {
  const p = join(out, f)
  const s = readFileSync(p, 'utf8')
  const t = s
    .replace(/new URL\('\.\/(\w+)\.ts', import\.meta\.url\)/g, "new URL('./$1.js', import.meta.url)")
    .replace(/(from|import) '(\.\/\w+)'/g, "$1 '$2.js'")
  if (t !== s) writeFileSync(p, t)
}
for (const f of ['vstbridge.js', 'vstbridge.d.ts', 'vstbridge_bg.wasm']) copyFileSync(join(web, f), join(out, f))
console.log(`dist/ に ${readdirSync(out).length} 個のファイルを書き出した`)
