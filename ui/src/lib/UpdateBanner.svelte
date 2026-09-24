<script lang="ts">
	import Download from 'lucide-svelte/icons/download';
	import { slide } from 'svelte/transition';
	import { trvani, zpomaleni } from './motion';
	import { runUpdate, updater } from './updater.svelte';

	// Upozornění na novou verzi — stejné jako ve WinSentu, jen v úzkém
	// okně na celou šířku dole a v toku stránky (ne plovoucí): přes
	// obsah by zakrylo spodek panelu, kde bude časem hlavní přepínač.
	//
	// Nemizí samo a nedá se odkliknout: stará verze je stav, který platí,
	// dokud se neaktualizuje. Křížek by z toho udělal oznámení, které si
	// člověk odbaví a zapomene.
</script>

<div class="upd" transition:slide={{ duration: trvani(160), easing: zpomaleni }}>
	<div class="upd-in">
		<Download size={17} />
		<div class="upd-text">
			<b>Je dostupná nová verze</b>
			<span class="upd-ver">
				máš <span class="mono">{updater.current}</span> · nová
				<span class="mono">{updater.latest}</span>
			</span>
			{#if updater.runError}
				<span class="upd-err">{updater.runError}</span>
			{/if}
		</div>
		<button class="upd-btn" disabled={updater.busy} onclick={() => void runUpdate()}>
			{updater.launched ? 'spouštím instalátor…' : updater.busy ? 'stahuji…' : 'Aktualizovat'}
		</button>
	</div>
</div>

<style>
	/* Vnější obal nese jen odsazení: `slide` animuje výšku a padding
	   prvku, takže rámeček s pozadím musí být až uvnitř, jinak by se
	   při vyjíždění deformoval. */
	.upd {
		flex-shrink: 0;
		padding: 8px 10px 0;
	}
	/* Jantarová, ne červená: není to porucha, jen je co stáhnout.
	   `wrap` + minimální šířka textu: dlouhý popisek tlačítka
	   („spouštím instalátor…") by v nejužším okně (380 px) text
	   zmáčkl tak, že by se čísla verzí lámala uprostřed. Tlačítko
	   pak radši spadne na vlastní řádek, doprava. */
	.upd-in {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 12px;
		padding: 11px 12px 11px 14px;
		border: 1px solid color-mix(in srgb, var(--warn) 45%, transparent);
		border-radius: var(--radius-lg);
		background: color-mix(in srgb, var(--warn) 12%, var(--panel));
		color: var(--text);
	}
	.upd-in > :global(svg) {
		flex: none;
		color: var(--warn);
	}
	.upd-text {
		flex: 1 1 10rem;
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
		font-size: var(--fs-sm);
	}
	.upd-ver {
		color: var(--text-dim);
		font-size: var(--fs-xs);
		overflow-wrap: anywhere;
	}
	.upd-ver .mono {
		font-family: var(--font-mono);
	}
	.upd-err {
		color: var(--danger);
		font-size: var(--fs-xs);
		overflow-wrap: anywhere;
	}
	.upd-btn {
		flex: none;
		margin-left: auto;
		padding: 7px 14px;
		border: none;
		border-radius: var(--radius-sm);
		background: var(--warn);
		color: #1b1200;
		font-size: var(--fs-sm);
		font-weight: 600;
		cursor: pointer;
		transition: filter var(--t-fast) var(--ease);
	}
	.upd-btn:hover:not(:disabled) {
		filter: brightness(1.12);
	}
	.upd-btn:disabled {
		opacity: 0.6;
		cursor: wait;
	}
</style>
