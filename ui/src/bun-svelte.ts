// Předehra `bun test` (bunfig.toml): moduly s runami (`*.svelte.ts`)
// přeloží kompilátor Svelte, jako při buildu Vite. Testy tak zkoušejí
// skutečné stavy okna (klávesy, pady) i pořadí událostí, ne jejich ruční
// kopii. Bez závislosti navíc — Svelte i Bun už jsou. Do buildu se
// nedostane: nikdo ho neimportuje.
import { file, plugin, Transpiler } from 'bun';
import { compileModule } from 'svelte/compiler';

// Svelte sám rozumí jen JavaScriptu — typy nejdřív odstraní Bun.
const ts = new Transpiler({ loader: 'ts' });

plugin({
	name: 'svelte-moduly',
	setup(build) {
		build.onLoad({ filter: /\.svelte\.ts$/ }, async ({ path }) => {
			const js = ts.transformSync(await file(path).text());
			const vysledek = compileModule(js, { filename: path, generate: 'client' });
			return { contents: vysledek.js.code, loader: 'js' };
		});
	}
});
