<script lang="ts">
	import { cilPrirazeni, efekty, klavesy, prirad, vyprazdni, zvyraznene } from './klavesy.svelte';
	import type { Klavesa } from './smlouva';
	import Vstup from './Vstup.svelte';
	import {
		cileKlaves,
		dalsiCile,
		indexy,
		jedinyVstup,
		klicKlavesy,
		PODKLADY,
		POLOHY,
		posunHlavicky,
		seskup,
		sviti,
		VSTUPY
	} from './vstupy';
	import { zive } from './zive.svelte';

	// Schéma ovladače ve tvaru Xboxu. Editor, živá detekce i náhled jsou
	// jedna plocha: každý vstup je čepička s klávesou, stisk ji rozsvítí,
	// klik na ni přiřadí jinou. Velikost se řídí šířkou karty (container
	// query) — v nejužším okně se schéma zmenší, nepřeteče.

	interface Props {
		/** Ovladač 0–3. */
		pad: number;
	}

	let { pad }: Props = $props();

	const skupiny = $derived(seskup(klavesy.vazby, pad));
	const bity = $derived(indexy(klavesy.vstupy));
	const jediny = $derived(jedinyVstup(klavesy.vazby));
	const z = $derived(zive.pady[pad]!);
	const cil = $derived(cilPrirazeni());
	const posunL = $derived(posunHlavicky(z.l[0], z.l[1]));
	const posunP = $derived(posunHlavicky(z.p[0], z.p[1]));
	// Kam všude patří každá klávesa — sdílená klávesa (Fáze 7, Z4) zoranžoví
	// čepičky všech svých vstupů, i na jiných ovladačích.
	const cile = $derived(cileKlaves(klavesy.vazby));

	/** Klávesy čepičky, které patří i jiným vstupům (klíče). */
	function sdilene(v: (typeof VSTUPY)[number], klavesy: readonly Klavesa[]): string[] {
		return klavesy.filter((k) => dalsiCile(cile, k, pad, v).length > 0).map(klicKlavesy);
	}

	/** Najetí myší: zvýraznit klávesy sdílené čepičky (jen při změně — přejíždění
	    po nesdílených čepičkách stav nepřepisuje). */
	function najeti(sdil: string[], najeto: boolean): void {
		const nove = najeto ? sdil : [];
		if (nove.length === 0 && zvyraznene.klice.length === 0) return;
		zvyraznene.klice = nove;
	}
</script>

<div class="schema">
	<div class="mrizka">
		{#each PODKLADY as p, i (i)}
			<span class="podklad {p.druh}" style:grid-area={p.oblast.join(' / ')} aria-hidden="true">
				{#if p.druh === 'kriz'}✚{/if}
			</span>
		{/each}
		{#each VSTUPY as v (v)}
			{@const pol = POLOHY[v]}
			{@const kl = skupiny[v] ?? []}
			{@const sdil = sdilene(v, kl)}
			<Vstup
				vstup={v}
				r={pol.r}
				s={pol.s}
				popisek={pol.popisek}
				hlavicka={!!pol.hlavicka}
				posun={pol.hlavicka === 'l' ? posunL : pol.hlavicka === 'p' ? posunP : undefined}
				klavesy={kl}
				dalsi={(k) => dalsiCile(cile, k, pad, v)}
				sdilena={sdil.length > 0}
				zvyraznena={sdil.some((k) => zvyraznene.klice.includes(k))}
				onnajeti={(najeto) => najeti(sdil, najeto)}
				sviti={sviti(z.drzi, z.hra, bity[v])}
				cil={cil?.pad === pad && cil.vstup === v}
				posledni={jediny?.pad === pad && jediny.vstup === v}
				efekt={efekty[`${pad}:${v}`]}
				onprirad={(pridat) => void prirad(pad, v, pridat)}
				onvyprazdni={() => void vyprazdni(pad, v)}
			/>
		{/each}
	</div>
</div>

<style>
	.schema {
		container-type: inline-size;
	}
	/* Čepička min(38 px, 10 % šířky), mezera do 1,1 %: 9 sloupců se vejde
	   vždy (9 × 10 + 8 × 1,1 < 100 %). */
	.mrizka {
		--cep: min(38px, 10cqi);
		--mezera: min(5px, 1.1cqi);
		display: grid;
		grid-template-columns: repeat(9, var(--cep));
		grid-template-rows: repeat(7, var(--cep));
		gap: var(--mezera);
		justify-content: center;
		padding: 4px 0 2px;
	}
	/* Bez container query (starý WebView2) pevná velikost — vejde se
	   i do nejužšího okna. */
	@supports not (width: 1cqi) {
		.mrizka {
			--cep: 30px;
			--mezera: 3px;
		}
	}

	/* Podklady skupin: jamka páčky a tlačítek, kříž D-padu. Jen tvar
	   Xboxu na pozadí — na klik nereagují. */
	.podklad {
		pointer-events: none;
	}
	.jamka {
		margin: calc(var(--cep) * -0.12);
		border: 1px solid var(--border);
		border-radius: 50%;
		background: rgba(255, 255, 255, 0.015);
	}
	.kriz {
		display: grid;
		place-items: center;
		color: var(--text-faint);
		font-size: calc(var(--cep) * 0.55);
		line-height: 1;
	}
</style>
