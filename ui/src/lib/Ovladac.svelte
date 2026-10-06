<script lang="ts">
	import { cilPrirazeni, efekty, klavesy, prirad, vyprazdni } from './klavesy.svelte';
	import Vstup from './Vstup.svelte';
	import { indexy, jedinyVstup, PODKLADY, POLOHY, posunHlavicky, seskup, sviti, VSTUPY } from './vstupy';
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
			<Vstup
				vstup={v}
				r={pol.r}
				s={pol.s}
				popisek={pol.popisek}
				hlavicka={!!pol.hlavicka}
				posun={pol.hlavicka === 'l' ? posunL : pol.hlavicka === 'p' ? posunP : undefined}
				klavesy={skupiny[v] ?? []}
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
